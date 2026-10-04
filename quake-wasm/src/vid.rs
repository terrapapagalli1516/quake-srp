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
//! - **Native** (`vid_native 1`, slop): the picture fills the page's box for
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

use crate::app::{ensure_app, App, APP, START_PRESET};

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

/// The most pixels a native frame may have: two frames of it fit the threads
/// build's fixed 512 MiB (`build.rs`), with room to spare.
///
/// Measured (2026-10-03, headless Chromium, a growable threads build, whose
/// memory's size is the heap's high-water mark; 1x on 8 threads, e1m1, e1m3,
/// e1m4 and e1m7 each turned all the way round): a page started at a size
/// holds about 26 MB and 19 bytes a pixel — 1920x920 56 MB, 3840x2000 145,
/// 5120x2720 286, 6400x3440 437, 7680x4160 623 (so 8K at 1x stops the
/// game). And a resize does not hand the old frame's memory to the next,
/// larger one: the heap then holds both (4800x2880 then 5120x3200: 618 MB,
/// where a page started at 5120x3200 takes 333). So the limit is what a
/// window's frame and its fullscreen's hold together: 12 million pixels,
/// about 26 + 2 x 19 x 12 = 482 MB of the 537. 4K (8.3 million) and a
/// 5120x2160 ultrawide (11.1) draw at 1x; 5K (14.7), 6K and 8K at 2x.
pub(crate) const MAX_FRAME_PIXELS: usize = 12_000_000;

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

/// How many device pixels a side make one of the picture's in native mode:
/// the setting (`vid_pixelsize`), or the next size up whose frame fits the
/// memory ([`MAX_FRAME_PIXELS`]) when a `win_w x win_h` box at the setting
/// would not — the player's own pick included, so a forced 1x on an 8K
/// screen draws at 2x instead of stopping the game. 4 at most.
pub(crate) fn pixel_size(cvars: &Cvars, window: (u32, u32)) -> u32 {
    let max = u32::from(quake_rs::cvar::PIXEL_SIZE_MAX);
    let fits = |&p: &u32| {
        let (w, h) = frame_size(window, p);
        w * h <= MAX_FRAME_PIXELS
    };
    (u32::from(cvars.pixel_size).clamp(1, max)..=max).find(fits).unwrap_or(max)
}

/// The native picture's size for a `win_w x win_h` box at pixel size `p`:
/// the box divided by it, at least 320x200, at most the renderer's hires
/// limit.
fn frame_size((win_w, win_h): (u32, u32), p: u32) -> (usize, usize) {
    let (w, h) = ((win_w / p) as usize, (win_h / p) as usize);
    (w.clamp(MIN_W as usize, render::HIRES_MAXWIDTH), h.clamp(MIN_H as usize, render::HIRES_MAXHEIGHT))
}

/// The picture's size for these settings: native, the page's box at the
/// pixel size [`pixel_size`] gives; otherwise the video mode, clamped.
pub(crate) fn picture_size(cvars: &Cvars, window: Option<(u32, u32)>) -> (usize, usize) {
    match window.filter(|_| cvars.native) {
        Some(win) => frame_size(win, pixel_size(cvars, win)),
        None => {
            let (w, h) = cvars.vid_resolution;
            clamp_resolution(i32::from(w), i32::from(h))
        }
    }
}

/// Whether the picture is shown native (the page fills its box, square
/// pixels) rather than as a mode in a 4:3 box.
pub(crate) fn native(a: &App) -> bool {
    a.settings.cvars.native && a.window.is_some()
}

/// Point Video Options at the live picture (`Menu::sync_resolution`): the
/// render size, [`native`] (not just `vid_native`'s cvar — a window must be
/// known too, or there's nothing to fill natively), whether the
/// native-resolution rows belong in the list at all (the slop preset, or a
/// native picture in Classic — `vid_native 1` from the console; otherwise
/// Classic's list is `RESOLUTION_PRESETS` alone), and the size each of them
/// gives ([`picture_size`] at 1x..4x, the memory limit included). Every
/// caller that used to hand `Menu::sync_resolution` the render size alone
/// goes through this now, so the facts can never drift out of sync with it.
pub(crate) fn sync_menu_resolution(a: &mut App) {
    let native = native(a);
    let native_rows = native || a.settings.preset == quake_rs::settings::Preset::Slop;
    let mut sizes = [(0, 0); quake_rs::menu::NATIVE_ROWS];
    if a.window.is_some() {
        for (p, size) in (1..).zip(&mut sizes) {
            let cvars = Cvars { native: true, pixel_size: p, ..a.settings.cvars.clone() };
            let (w, h) = picture_size(&cvars, a.window);
            *size = (w as i32, h as i32);
        }
    }
    let (w, h) = (a.render_w as i32, a.render_h as i32);
    a.menu.sync_resolution(w, h, native, native_rows, sizes);
}

/// Once a frame, before the client frame: the framebuffer to the size the
/// settings ask for, and the 2-D layer's scale (a setting `draw` still keeps
/// per thread). The renderer's settings go with the frame, in [`vid`].
pub(crate) fn apply_settings(a: &mut App) {
    let (w, h) = picture_size(&a.settings.cvars, a.window);
    a.set_render_size(w, h);
    quake_rs::draw::set_scaled_2d(a.settings.cvars.scaled_2d);
    sync_menu_resolution(a);
}

/// The checks' and the benchmark's shorthand for the picture (the
/// `set_video` call): `modern` is the slop preset's — native resolution at
/// one device pixel a pixel, Hor+, the fluid sky, the gliding light styles,
/// the flickering torches, the preset's perspective span (8):
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
        c.persp_span = if modern { Cvars::slop().persp_span } else { Cvars::classic().persp_span };
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
pub(crate) fn set_window(w: u32, h: u32) {
    ensure_app(|a| a.window = (w > 0 && h > 0).then_some((w, h)));
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
/// Read-only, for the page/verification harness. Before the App exists, the
/// start preset's ([`START_PRESET`]: slop's one step past id's 100).
pub(crate) fn viewsize() -> f32 {
    APP.with(|c| c.borrow().as_ref().map_or_else(|| START_PRESET.viewsize(), |a| a.settings.cvars.viewsize))
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

    /// The picture is the window over the pixel size, any aspect, at least
    /// 320x200; Classic's is the video mode, whatever the window.
    #[test]
    fn native_pictures_are_whole_fractions_of_the_window() {
        let mut c = quake_rs::cvar::Cvars::slop();
        let size = |c: &Cvars, win| (picture_size(c, Some(win)), pixel_size(c, win));
        assert_eq!(size(&c, (1920, 1080)), ((1920, 1080), 1));
        assert_eq!(size(&c, (2560, 1440)), ((2560, 1440), 1), "1x is 1x: no budget, no guess");
        assert_eq!(size(&c, (1000, 640)), ((1000, 640), 1), "any aspect");
        assert_eq!(size(&c, (300, 150)), ((320, 200), 1), "at least 320x200");
        c.pixel_size = 3;
        assert_eq!(size(&c, (1920, 1080)), ((640, 360), 3));
        c.pixel_size = 2;
        assert_eq!(size(&c, (2640, 1080)), ((1320, 540), 2), "a phone's 2x");
        c.native = false;
        assert_eq!(picture_size(&c, Some((1920, 1080))), (960, 600), "the mode");
        assert_eq!(picture_size(&c, None), (960, 600));
    }

    /// The renderer's sky follows `r_fluidsky`: off in Classic, on in slop,
    /// and `set_video`'s `modern` is `quaketool --video modern`'s, the fluid
    /// sky with the rest.
    #[test]
    fn the_sky_follows_r_fluidsky() {
        let sky = || APP.with(|c| vid(c.borrow().as_ref().unwrap()).video.sky);
        assert_eq!(boot(), 1);
        assert_eq!(sky(), SkyScroll::Classic, "the tests start in Classic");
        use_slop();
        assert_eq!(sky(), SkyScroll::Fluid);
        crate::host_cmd::execute_console_command("r_fluidsky 0");
        assert_eq!(sky(), SkyScroll::Classic);
        assert_eq!(set_video("modern"), 1);
        assert_eq!(sky(), SkyScroll::Fluid);
        assert_eq!(set_video("classic"), 1);
        assert_eq!(sky(), SkyScroll::Classic);
    }

    /// The renderer's perspective follows `r_perspspan`: id's 16-pixel spans
    /// in Classic, every 8 in slop (the user's call: at 1080p and above the
    /// 16-pixel affine steps show; 8 is id's own portable-C loop), the other
    /// spans and exact when set; the old `wasm_exactpersp` sets its two ends;
    /// and `set_video`'s `modern` is `quaketool --video modern`'s, the
    /// preset's span with the rest.
    #[test]
    fn the_perspective_follows_r_perspspan() {
        let span = || APP.with(|c| vid(c.borrow().as_ref().unwrap()).persp_span);
        assert_eq!(boot(), 1);
        assert_eq!(span(), PerspSpan::Spans16, "the tests start in Classic: id's spans");
        use_slop();
        assert_eq!(span(), PerspSpan::Spans8, "slop: id's portable C loop, every 8 pixels");
        for (line, want) in [("r_perspspan 4", PerspSpan::Spans4), ("r_perspspan 1", PerspSpan::Exact),
                             ("r_perspspan 64", PerspSpan::Spans64), ("r_perspspan 32", PerspSpan::Spans32),
                             ("wasm_exactpersp 0", PerspSpan::Spans16), ("wasm_exactpersp 1", PerspSpan::Exact),
                             ("r_perspspan 16", PerspSpan::Spans16), ("r_perspspan 8", PerspSpan::Spans8)] {
            crate::host_cmd::execute_console_command(line);
            assert_eq!(span(), want, "{line}");
        }
        assert_eq!(set_video("classic"), 1);
        assert_eq!(set_video("modern"), 1);
        assert_eq!(span(), PerspSpan::Spans8);
        assert_eq!(set_video("classic"), 1);
        assert_eq!(span(), PerspSpan::Spans16);
    }

    /// The light styles the client animates follow `r_lerplightstyles` the
    /// same way: off in Classic, on in slop, and in `set_video`'s `modern`.
    #[test]
    fn the_light_styles_follow_r_lerplightstyles() {
        let lerp = || APP.with(|c| vid(c.borrow().as_ref().unwrap()).video.lightstyles);
        assert_eq!(boot(), 1);
        assert_eq!(lerp(), LerpLightStyles::Classic, "the tests start in Classic");
        use_slop();
        assert_eq!(lerp(), LerpLightStyles::Smooth);
        crate::host_cmd::execute_console_command("r_lerplightstyles 0");
        assert_eq!(lerp(), LerpLightStyles::Classic);
        assert_eq!(set_video("modern"), 1);
        assert_eq!(lerp(), LerpLightStyles::Smooth);
        assert_eq!(set_video("classic"), 1);
        assert_eq!(lerp(), LerpLightStyles::Classic);
    }

    /// The steady torches' flicker follows `r_torchflicker`, a strength: off
    /// in Classic, on in slop and `set_video`'s `modern`, any value between 0
    /// and 2 from the console (the user tunes it by eye), read back as set.
    #[test]
    fn the_torches_follow_r_torchflicker() {
        let torches = || APP.with(|c| vid(c.borrow().as_ref().unwrap()).video.torches);
        assert_eq!(boot(), 1);
        assert_eq!(torches(), TorchFlicker::OFF, "the tests start in Classic");
        use_slop();
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

    /// No frame is bigger than the memory holds ([`MAX_FRAME_PIXELS`]): the
    /// next pixel size that fits instead, for the machine's number and for
    /// the player's own pick alike — and never a frame past it at any
    /// window the renderer draws.
    #[test]
    fn no_frame_is_bigger_than_the_memory_holds() {
        let mut c = quake_rs::cvar::Cvars::slop();
        assert_eq!(pixel_size(&c, (7680, 4320)), 2, "8K at 1x would not fit: 2x");
        assert_eq!(picture_size(&c, Some((7680, 4320))), (3840, 2160));
        assert_eq!(pixel_size(&c, (5120, 2880)), 2, "5K: 2x");
        assert_eq!(pixel_size(&c, (3840, 2160)), 1, "4K: 1x");
        assert_eq!(pixel_size(&c, (5120, 2160)), 1, "a 5K ultrawide: 1x");
        c.pixel_size = 3;
        assert_eq!(pixel_size(&c, (7680, 4320)), 3, "a size that fits is what it says");
        for (w, h) in [(1920, 1080), (3840, 2160), (5120, 2880), (6016, 3384), (7680, 4320), (15360, 8640)] {
            for p in 1..=quake_rs::cvar::PIXEL_SIZE_MAX {
                c.pixel_size = p;
                let (fw, fh) = picture_size(&c, Some((w, h)));
                assert!(fw * fh <= MAX_FRAME_PIXELS, "{w}x{h} at {p}x: {fw}x{fh}");
                assert!(pixel_size(&c, (w, h)) >= u32::from(p), "never finer than asked");
            }
        }
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
        menu_down(); // -> 2 (Reset to slop)
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
