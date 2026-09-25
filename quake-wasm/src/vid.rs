//! Video — the mode side of vid_win.c (`VID_SetMode`'s runtime resolution,
//! clamped to a safe envelope; `vid.buffer` as the RGBA framebuffer export)
//! and the bits of screen.c the page reads or both client frames share (the
//! `viewsize` cvar, `Draw_TileClear`'s backtile).

use quake_rs::render;
use quake_rs::wad::Qpic;

use crate::app::{ensure_app, APP};

/// The default (boot) render resolution. A crisp `960x600` (preset index 4 — must
/// stay a member of [`render::RESOLUTION_PRESETS`] so the Video Options list
/// can mark it current). The page restores the player's *saved* resolution from
/// `localStorage` over this on load, and Options > Video Options lets them change
/// it at runtime; the chosen size now persists across boots / New Game / reloads. The
/// menu + HUD are drawn at their own pixel size, as WinQuake draws them in every
/// mode, unless the [`set_scaled_2d`] extra blows them up.
pub(crate) const DEFAULT_W: usize = 960;
pub(crate) const DEFAULT_H: usize = 600;
/// Sane bounds for [`set_resolution`] (and the menu presets): the framebuffer is
/// clamped to this envelope and its total pixel count capped so a runaway value
/// cannot allocate gigabytes. `1280*800*4` bytes ≈ 4 MB is the upper bound.
const MIN_W: i32 = 320;
const MAX_W: i32 = 1280;
const MIN_H: i32 = 200;
const MAX_H: i32 = 800;
const MAX_PIXELS: i32 = 1280 * 800;

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

/// The current render width in pixels (defaults to [`DEFAULT_W`] = 960). The page
/// reads this each frame and resizes its canvas backing store + ImageData when it
/// changes (e.g. after the Options menu picks a different preset), and persists it
/// to `localStorage` so the choice survives a reload.
#[no_mangle]
pub extern "C" fn width() -> i32 {
    APP.with(|c| c.borrow().as_ref().map(|a| a.render_w as i32).unwrap_or(DEFAULT_W as i32))
}
/// The current render height in pixels (defaults to [`DEFAULT_H`] = 600).
#[no_mangle]
pub extern "C" fn height() -> i32 {
    APP.with(|c| c.borrow().as_ref().map(|a| a.render_h as i32).unwrap_or(DEFAULT_H as i32))
}

/// Set the render resolution at runtime, reallocating the framebuffer. The
/// requested `(w, h)` is clamped to the supported envelope (width 320..=1280,
/// height 200..=800, and total pixels <= 1_280*800 so a runaway can't OOM) via
/// [`clamp_resolution`]; out-of-range input is clamped, never a panic. After this,
/// `width()`/`height()` report the new (clamped) size and the next `step` renders
/// the scene at it. The menu + HUD are drawn at their own pixel size, as in
/// WinQuake (blown up to the framebuffer with the [`set_scaled_2d`] extra).
#[no_mangle]
pub extern "C" fn set_resolution(w: i32, h: i32) {
    let (cw, ch) = clamp_resolution(w, h);
    ensure_app(|a| {
        a.set_render_size(cw, ch);
        // Keep the Video Options "current mode" pointing at the new size too, so
        // a programmatic set (e.g. the page restoring a saved resolution on load)
        // doesn't leave the menu showing a stale mode.
        a.menu.sync_resolution(cw as i32, ch as i32);
    });
}

/// The width:height ratio the page DISPLAYS the canvas at, whatever its backing
/// store: `web/index.html` shows it in a 640x480 box, and with `aspect-ratio:
/// 4/3` in fullscreen and on narrow screens — as DOS and Windows Quake's modes
/// filled a 4:3 monitor. Every resolution preset is 16:10, so its pixels are
/// shown 1.2x taller than wide.
pub(crate) const DISPLAY_ASPECT: f64 = 4.0 / 3.0;

/// How the renderer draws the 3-D view `vrect` of a `render_w x render_h` frame:
/// `vid.aspect` for that mode on the page's [`DISPLAY_ASPECT`] (vid_win.c's
/// `(h/w)*(320/240)`: 0.8333 at every 16:10 preset), which `R_ViewChanged`
/// folds into the projection so the world is not stretched by the 4:3 display;
/// where the view sits on that screen (`D_Sky_uv_To_st` centres the sky on the
/// screen); and the port's renderer extras, off unless switched on (Options >
/// Web extras, `wasm_*`; [`crate::extras::extras`]).
pub(crate) fn render_options(
    vrect: &render::ViewRect,
    render_w: usize,
    render_h: usize,
) -> render::RenderOptions {
    render::RenderOptions {
        pixel_aspect: render::vid_aspect(render_w, render_h, DISPLAY_ASPECT),
        screen: Some(render::ScreenPlace { x: vrect.x, y: vrect.y, vid_w: render_w, vid_h: render_h }),
        exact_perspective: crate::extras::extras().exact_persp,
    }
}

/// The `viewsize` cvar (Options "Screen size", `sizeup`/`sizedown`), 30..=120.
/// Read-only, for the page/verification harness like [`volume`].
#[no_mangle]
pub extern "C" fn viewsize() -> f32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| a.menu.viewsize())
            .unwrap_or(render::VIEWSIZE_DEFAULT)
    })
}

/// Set the `viewsize` cvar, bounded to 30..=120 (the console's `viewsize n`):
/// the page restoring the size it saved, as `Host_WriteConfiguration`'s
/// config.cfg carries `viewsize` across sessions in id's Quake.
#[no_mangle]
pub extern "C" fn set_viewsize(v: f32) {
    ensure_app(|a| a.menu.set_viewsize(v));
}

/// The "scaled 2-D" extra (not id; off by default): `1` draws the status bar,
/// menus, console and text as id's 320x200 screen blown up to fill the
/// framebuffer, `0` at their own pixel size as WinQuake does in every mode
/// ([`quake_rs::draw::set_scaled_2d`]). For the page's extras; takes effect
/// on the next frame.
#[no_mangle]
pub extern "C" fn set_scaled_2d(on: i32) {
    quake_rs::draw::set_scaled_2d(on != 0);
}

/// `1` while the "scaled 2-D" extra is on ([`set_scaled_2d`]).
#[no_mangle]
pub extern "C" fn scaled_2d() -> i32 {
    quake_rs::draw::scaled_2d() as i32
}

#[no_mangle]
pub extern "C" fn framebuffer() -> *const u8 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| a.fb.as_ptr())
            .unwrap_or(std::ptr::null())
    })
}

/// The `backtile` pic (`draw_backtile`, gfx.wad) for [`render::compose_view`],
/// fetched only when the 3-D view leaves part of the screen to tile-clear
/// (viewsize below 120). `None` when the view covers the whole frame or the
/// wad lacks it (then the border fills black).
pub(crate) fn backtile_for(
    vrect: &render::ViewRect,
    render_w: usize,
    render_h: usize,
    gfx_wad: Option<&quake_rs::wad::Wad2>,
) -> Option<Qpic> {
    if vrect.w == render_w && vrect.h == render_h {
        return None;
    }
    gfx_wad.and_then(|g| g.qpic("backtile").ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::boot;
    use crate::console::console_toggle;
    use crate::host::step;
    use crate::menu::{menu_cancel, menu_down, menu_left, menu_right, menu_select, menu_visible};
    use crate::test_util::*;

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

        // A valid in-range resolution is applied verbatim; width()/height() follow
        // and the framebuffer is exactly w*h*4 bytes.
        set_resolution(640, 400);
        assert_eq!(width(), 640);
        assert_eq!(height(), 400);
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().expect("app exists after set_resolution");
            assert_eq!(a.render_w, 640);
            assert_eq!(a.render_h, 400);
            assert_eq!(a.fb.len(), 640 * 400 * 4, "framebuffer reallocated to 640*400*4");
        });

        // Out-of-range input is clamped, not panicked: a huge request lands within
        // the envelope and the fb matches the clamped size.
        set_resolution(100000, 100000);
        let (w, h) = (width(), height());
        assert!((MIN_W..=MAX_W).contains(&w) && (MIN_H..=MAX_H).contains(&h));
        assert!(w.saturating_mul(h) <= MAX_PIXELS);
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            assert_eq!(a.fb.len(), (w as usize) * (h as usize) * 4);
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
            assert_eq!(a.fb.len(), DEFAULT_W * DEFAULT_H * 4, "default fb is DEFAULT_W*DEFAULT_H*4");
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
            assert_eq!(a.fb.len(), 1280 * 800 * 4, "step renders into the 1280*800 framebuffer");
            // Every alpha byte is 255 (step pushes opaque RGBA), proving the whole
            // larger buffer was painted, not just the smaller default region.
            assert!(a.fb.chunks_exact(4).all(|px| px[3] == 255), "full fb painted opaque");
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
            assert_eq!(a.fb.len(), 1120 * 700 * 4, "fb reallocated to the new mode");
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
            let px = a.fb.chunks_exact(4).map(|p| [p[0], p[1], p[2]]).collect();
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
