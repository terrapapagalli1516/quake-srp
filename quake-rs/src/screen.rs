//! Screen layout: the view rectangle, the composed frame, centre prints.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/screen.c` — `SCR_CalcRefdef` (with `R_SetVrect`, `r_main.c`),
//! `SCR_UpdateScreen`'s tile-cleared border, `SCR_DrawCenterString`,
//! `SCR_ScreenShot_f`/`WritePCXfile`.

use crate::draw::{blit_qpic_at, draw_char_scaled, draw_string_scaled, draw_tile_clear, screen_2d};
use crate::render::Image;

// ---------------------------------------------------------------------------
// Screen layout: scr_viewsize -> the 3-D view rectangle + sb_lines
// (SCR_CalcRefdef / R_SetVrect / Draw_TileClear)
// ---------------------------------------------------------------------------

/// `scr_viewsize` ("viewsize", screen.c: default "100", archived) and its
/// bounds: SCR_CalcRefdef clamps it to 30..=120 and `M_AdjustSliders` /
/// `sizeup` / `sizedown` move it in steps of 10. 100 is the full-width view
/// above the full status bar; 110 drops the inventory strip; 120 drops the
/// status bar entirely; below 100 the view shrinks, centred, inside a
/// `backtile` border.
pub const VIEWSIZE_DEFAULT: f32 = 100.0;
pub const VIEWSIZE_MIN: f32 = 30.0;
pub const VIEWSIZE_MAX: f32 = 120.0;
pub const VIEWSIZE_STEP: f32 = 10.0;

/// `sb_lines` for the full status bar: the 24-row `sbar` plus the 24-row
/// `ibar` inventory strip (SCR_CalcRefdef's `24+16+8`).
pub const SB_LINES_FULL: i32 = 48;

/// A rectangle of the framebuffer, in pixels (`vrect_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewRect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

/// `vid.aspect` for a `vid_w x vid_h` mode displayed with the width:height
/// ratio `display_aspect` — `vid_win.c`/`vid_x.c`'s
/// `((float)vid.height / (float)vid.width) * (320.0 / 240.0)` when the display
/// is a 4:3 monitor (`display_aspect` 4/3). It is the width of a displayed
/// pixel over its height, the `pixelAspect` of `R_ViewChanged`
/// ([`RenderOptions::pixel_aspect`](crate::render::RenderOptions::pixel_aspect)):
/// 0.8333 for 320x200 or 1280x800 on 4:3 (tall pixels), 1.0 for 640x480. The
/// arithmetic is the C's: a float ratio times the double constant, stored to
/// float.
pub fn vid_aspect(vid_w: usize, vid_h: usize, display_aspect: f64) -> f32 {
    if vid_w == 0 || vid_h == 0 || !(display_aspect.is_finite() && display_aspect > 0.0) {
        return 1.0;
    }
    ((vid_h as f32 / vid_w as f32) as f64 * display_aspect) as f32
}

/// How the 3-D view meets the status bar: the `scr_sbaroverlay` setting
/// ([`Cvars::sbar_layout`](crate::cvar::Cvars::sbar_layout)), which
/// [`calc_refdef`] and [`warp_vrect`] lay the view out by and
/// [`draw_hud_into`](crate::sbar::draw_hud_into) draws the bar by.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SbarLayout {
    /// id's: `SCR_CalcRefdef` stops the view above the status bar's rows, and
    /// on a screen wider than the bar `Sbar_Draw` tile-clears either side of
    /// it with `backtile`.
    #[default]
    Classic,
    /// A 2026 extra: the view takes the bar's rows too, as at viewsize 120,
    /// and the bar is drawn over its bottom — QuakeSpasm's `scr_sbaralpha`
    /// layout (its `SCR_CalcRefdef` counts no `sb_lines` once the bar is see-
    /// through), with the bar opaque. The bar's pictures cover their 320
    /// columns as before (`sbar`, `ibar` and `scorebar` have no transparent
    /// texel), so only its sides change: they show the game, not the
    /// backtile. `sb_lines` and the bar's rows ([`status_bar_rows`]) are the
    /// same in both layouts; below viewsize 100 the view keeps its size and is
    /// centred on the whole screen (QuakeSpasm's too), the bar over the
    /// border, or over the view's own bottom rows when it reaches them (90).
    Overlay,
}

impl SbarLayout {
    /// `R_SetVrect`'s `lineadj`: of a status bar `bar_rows` framebuffer rows
    /// tall, the rows the 3-D view must stay above — all of them in id's
    /// layout, none when the bar is drawn over the view.
    fn lineadj(self, bar_rows: i64) -> i64 {
        match self {
            SbarLayout::Classic => bar_rows,
            SbarLayout::Overlay => 0,
        }
    }
}

/// What `SCR_CalcRefdef` works out each time the view changes: where the 3-D
/// view goes (`r_refdef.vrect`) and how many status-bar lines are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refdef {
    /// `r_refdef.vrect`: the 3-D view rectangle in framebuffer pixels. The
    /// renderer draws into a `vrect.w x vrect.h` image with the projection
    /// centred on it and `fov_x` spanning its width — R_ViewChanged's
    /// `xcenter = vrect.width/2 + vrect.x`, `xscale = vrect.width / (2 tan(fov_x/2))`,
    /// `yscale = xscale * pixelAspect` ([`vid_aspect`]); the software renderer derives its
    /// vertical extent from that, not from CalcFov's `fov_y`.
    pub vrect: ViewRect,
    /// `sb_lines` in 2-D screen rows ([`crate::draw::screen_2d`]): 48 (sbar +
    /// inventory), 24 (sbar only) or 0 (no status bar).
    pub sb_lines: i32,
}

/// `SCR_CalcRefdef` + `R_SetVrect` (screen.c / r_main.c) for a `vid_w x vid_h`
/// framebuffer: bound `viewsize` to 30..=120, pick `sb_lines` (an intermission
/// is always full screen: `size = 120`), then size the view rectangle —
/// `viewsize`% of the screen (100 at most), at least 96 wide, width a multiple
/// of 8 and height even, never taller than the screen minus the status bar,
/// centred horizontally on the screen and vertically in the space above the
/// status bar.
///
/// The status bar the view must clear (`lineadj`) is `sb_lines` 2-D rows,
/// which the "scaled 2-D" extra ([`crate::draw::set_scaled_2d`]) makes
/// `sb_lines * scale` framebuffer rows — exactly the rows
/// [`draw_hud_into`](crate::sbar::draw_hud_into) paints. With
/// [`SbarLayout::Overlay`] it is none: the view is laid out on the whole
/// screen, as at viewsize 120 but for viewsize's own scaling below 100, and
/// `sb_lines` is unchanged. With [`SbarLayout::Classic`] and without the
/// "scaled 2-D" extra every number is the C's, in every mode.
///
/// The arithmetic keeps the C's types: `size` is a `float`, the products are
/// truncated to `int` (so e.g. 70% of 320 is `(int)(320 * 0.7f) = 224`, as an
/// IEEE-single build computes it).
pub fn calc_refdef(vid_w: usize, vid_h: usize, viewsize: f32, intermission: bool, sbar: SbarLayout) -> Refdef {
    let (viewsize, sb_lines, bar_rows) = status_lines(vid_w, vid_h, viewsize, intermission);
    let lineadj = sbar.lineadj(bar_rows);
    Refdef { vrect: set_vrect(vid_w as i64, vid_h as i64, viewsize, lineadj, intermission), sb_lines }
}

/// The framebuffer rows, bottom-anchored, the status bar covers this frame:
/// what anything drawn outside the renderer must keep clear, so it never sits
/// over the HUD's numbers and icons ([`crate::sbar::draw_hud_into`]) — the
/// browser's touch layout (`web/touch.js`, the `sbar_height` automation call)
/// is the one caller today. The same in either [`SbarLayout`]: in id's,
/// [`calc_refdef`] keeps the 3-D view above these rows; with the bar over the
/// view they are still the bar's. 0 with no status bar (viewsize 120, or any
/// viewsize during an intermission, which is always full screen).
pub fn status_bar_rows(vid_w: usize, vid_h: usize, viewsize: f32, intermission: bool) -> i64 {
    status_lines(vid_w, vid_h, viewsize, intermission).2
}

/// SCR_CalcRefdef's first half: the bounded `viewsize` (a non-number reads as
/// the default), `sb_lines`, and the framebuffer rows the status bar covers
/// (`sb_lines` scaled like the 2-D layer, see [`calc_refdef`]: in id's layout
/// the `lineadj` R_SetVrect keeps the view above).
fn status_lines(vid_w: usize, vid_h: usize, viewsize: f32, intermission: bool) -> (f32, i32, i64) {
    let viewsize = if viewsize.is_finite() {
        viewsize.clamp(VIEWSIZE_MIN, VIEWSIZE_MAX)
    } else {
        VIEWSIZE_DEFAULT
    };
    // "intermission is always full screen"
    let size = if intermission { 120.0 } else { viewsize };
    let sb_lines = if size >= 120.0 {
        0 // no status bar at all
    } else if size >= 110.0 {
        24 // no inventory
    } else {
        SB_LINES_FULL
    };
    // The status bar's framebuffer rows: sb_lines 2-D rows, bottom-anchored
    // (ceil(sb_lines * scale) under the "scaled 2-D" extra).
    let scale = screen_2d(vid_w, vid_h).scale;
    let lineadj = ((sb_lines as f32 * scale).ceil() as i64).clamp(0, vid_h as i64);
    (viewsize, sb_lines, lineadj)
}

/// `R_SetVrect` (r_main.c): the view rectangle inside a `vw x vh` rectangle
/// for a bounded `viewsize`, kept above `lineadj` status-bar rows.
fn set_vrect(vw: i64, vh: i64, viewsize: f32, lineadj: i64, intermission: bool) -> ViewRect {
    let mut lineadj = lineadj;
    let mut size: f32 = if viewsize > 100.0 { 100.0 } else { viewsize };
    if intermission {
        size = 100.0;
        lineadj = 0;
    }
    size /= 100.0;
    let h = vh - lineadj;
    let mut w = (vw as f32 * size) as i64;
    if w < 96 {
        size = (96.0 / vw.max(1) as f64) as f32;
        w = 96; // min for icons
    }
    w &= !7;
    let mut height = (vh as f32 * size) as i64;
    if height > vh - lineadj {
        height = vh - lineadj;
    }
    height &= !1;
    // (A frame narrower than 96/8 px never occurs in the C; clamp so a tiny
    // test framebuffer still yields an in-bounds rectangle.)
    let w = w.clamp(0, vw);
    let height = height.clamp(0, vh);
    let x = ((vw - w) / 2).max(0);
    let y = ((h - height) / 2).max(0);
    ViewRect { x: x as usize, y: y as usize, w: w as usize, h: height as usize }
}

/// `WARP_WIDTH` x `WARP_HEIGHT` (d_iface.h): the size of `r_warpbuffer`, the
/// most the underwater view is rendered at.
pub const WARP_WIDTH: usize = 320;
pub const WARP_HEIGHT: usize = 200;

/// The view rectangle id renders an UNDERWATER frame into (`R_SetupFrame`,
/// r_misc.c, with `r_dowarp`): the view is drawn into `r_warpbuffer` — a
/// screen of at most [`WARP_WIDTH`] x [`WARP_HEIGHT`] — and `D_WarpScreen`
/// then stretches it over the screen's view rectangle ([`calc_refdef`]'s
/// `vrect`) while it wobbles it. A mode no larger than 320x200 renders at its
/// own size (the stretch is 1:1). A larger one is scaled down to 320 wide,
/// then capped at 200 high, and R_SetVrect runs on that with the status-bar
/// lines scaled by the same factor: `(int)(sb_lines * (h / vid.height))`
/// (the 2-D layer's `lineadj`: the C's own unless the "scaled 2-D" extra is
/// on, and none with [`SbarLayout::Overlay`]). At every 16:10
/// mode — all of [`RESOLUTION_PRESETS`](crate::menu::RESOLUTION_PRESETS) —
/// that is the 320x200 screen's own view rectangle.
///
/// id's height cap is `h = maxwarpheight; w *= maxwarpheight / h`, a ratio
/// of 1 (w stays 320), and R_ViewChanged's pixel aspect
/// (`vid.aspect * (h / w) * (vid.width / vid.height)`) undoes the squeeze.
/// The port projects square pixels, so for a mode taller than 16:10 it
/// narrows the buffer instead (`w = vid.width * 200 / vid.height`): the same
/// picture, sampled a little coarser across (e.g. 266 columns for 4:3). At
/// 16:10 and wider the C's aspect is `vid.aspect` itself and nothing differs.
///
/// With the hires extra (`hires`, [`VideoCvars::hires`](crate::render::VideoCvars))
/// there is no warp buffer: the underwater view is rendered at the screen's
/// view rectangle, as above water, and [`Renderer::warp_into`](crate::render::Renderer::warp_into)
/// scales the wobble to it — at 4K id's buffer would be blown up twelve times.
pub fn warp_vrect(vid_w: usize, vid_h: usize, viewsize: f32, intermission: bool, sbar: SbarLayout, hires: bool) -> ViewRect {
    let (viewsize, _, bar_rows) = status_lines(vid_w, vid_h, viewsize, intermission);
    let lineadj = sbar.lineadj(bar_rows);
    if (vid_w <= WARP_WIDTH && vid_h <= WARP_HEIGHT) || hires {
        return set_vrect(vid_w as i64, vid_h as i64, viewsize, lineadj, intermission);
    }
    let (mut w, mut h) = (vid_w as f32, vid_h as f32);
    if w > WARP_WIDTH as f32 {
        h *= WARP_WIDTH as f32 / w;
        w = WARP_WIDTH as f32;
    }
    if h > WARP_HEIGHT as f32 {
        h = WARP_HEIGHT as f32;
        // id: `w *= maxwarpheight / h` after that store, i.e. w stays 320 (see
        // above); a 16:10 mode whose float h lands a hair over 200 keeps 320.
        w = w.min((vid_w * WARP_HEIGHT / vid_h.max(1)) as f32);
    }
    let lineadj = (lineadj as f32 * (h / vid_h as f32)) as i64;
    set_vrect(w as i64, h as i64, viewsize, lineadj, intermission)
}

/// A `vid_w x vid_h` screen with everything outside the view rectangle
/// `vrect` tile-cleared ([`draw_tile_clear`] — SCR_UpdateScreen's
/// `Draw_TileClear(0,0,vid.width,vid.height)` under the view) and the
/// rectangle itself left for the view to be drawn into
/// ([`Renderer::render_into`](crate::render::Renderer::render_into)). The
/// status bar is drawn over the result afterwards, as in the C.
///
/// The tile only goes where the view does not — the four bands around it —
/// so, the view drawn, every screen pixel is written exactly once: the
/// screen is a spare frame buffer left uncleared (`Image::reused_uncleared`).
pub fn screen_with_backtile(
    vrect: ViewRect,
    vid_w: usize,
    vid_h: usize,
    backtile: Option<&crate::wad::Qpic>,
) -> Image {
    let mut img = Image::reused_uncleared(vid_w, vid_h);
    // The rectangle the view covers, clipped to the screen: [x0, x1) x [y0, y1).
    let x0 = vrect.x.min(vid_w);
    let x1 = x0 + vrect.w.min(vid_w - x0);
    let y0 = vrect.y.min(vid_h);
    let y1 = y0 + vrect.h.min(vid_h - y0);
    // The tile everywhere else: above, below, then either side.
    draw_tile_clear(&mut img, backtile, 0, 0, vid_w, y0);
    draw_tile_clear(&mut img, backtile, 0, y1, vid_w, vid_h - y1);
    draw_tile_clear(&mut img, backtile, 0, y0, x0, y1 - y0);
    draw_tile_clear(&mut img, backtile, x1, y0, vid_w - x1, y1 - y0);
    img
}

/// Put a rendered 3-D `view` (a `vrect.w x vrect.h` image) into a
/// `vid_w x vid_h` screen at `vrect`, with everything outside it tile-cleared
/// ([`screen_with_backtile`]). A view that already IS the whole screen
/// (viewsize 120, an intermission) comes back untouched, at zero cost. The
/// view is copied in runs of rows on up to `threads` threads (the bytes are
/// the same for any), and its buffer goes back to the pool
/// ([`crate::render::recycle_image`]). The client draws its view straight into
/// the screen instead; this is for a view drawn apart.
pub fn compose_view(
    view: Image,
    vrect: ViewRect,
    vid_w: usize,
    vid_h: usize,
    backtile: Option<&crate::wad::Qpic>,
    threads: usize,
) -> Image {
    if vrect.x == 0 && vrect.y == 0 && view.w == vid_w && view.h == vid_h {
        return view;
    }
    let mut img = screen_with_backtile(ViewRect { w: view.w, h: view.h, ..vrect }, vid_w, vid_h, backtile);
    img.blit(&view, vrect.x, vrect.y, threads);
    crate::render::recycle_image(view);
    img
}

/// `SCR_DrawCenterString`'s `y = vid.height*0.35` for a short message. The
/// double 0.35 is a hair under 0.35 and x86 Quake multiplies in the x87's
/// 64-bit mantissa (Sys_HighFPPrecision outside the 3-D view), where the
/// product is exact: when `height*0.35` is a whole number the truncation
/// lands one row higher — 69 on a 200-line screen, not 70 (measured: the
/// oracle's x87 build draws row 69). In f64 the product rounds up to 70.0,
/// so this is the exact floor in integers: `floor(height*0.35 - epsilon)`.
pub(crate) fn center_string_top(vid_h: i32) -> i32 {
    (vid_h * 7 - 1).div_euclid(20)
}

/// `SCR_DrawCenterString` (screen.c) in its finale mode: the centered text block
/// revealed one character at a time. `remaining` is the C's
/// `scr_printspeed.value * (cl.time - scr_centertime_start)` budget — note the
/// `if (!remaining--) return;` runs AFTER each `Draw_Character`, so a budget of
/// `n` paints `n + 1` characters (one appears the instant the finale starts).
/// A NEGATIVE budget paints the whole string: `!remaining--` only fires when
/// `remaining` is exactly 0 at the check, and a below-zero value just keeps
/// decrementing past it. Each line is scanned at most 40 characters (longer
/// lines truncate and skip to the next `\n`), the block starts at
/// `vid.height*0.35` for <= 4 lines else 48, and every line centers
/// independently — all verbatim from the C.
pub fn draw_center_string_revealed(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    text: &str,
    remaining: i32,
) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let sc = screen_2d(image.w, image.h);
    let lines: Vec<&str> = text.split('\n').collect();
    // scr_center_lines <= 4 => y = vid.height*0.35; taller => 48.
    let mut vy = if lines.len() <= 4 { center_string_top(sc.h) as f32 } else { 48.0 };
    let mut budget = remaining;
    for line in lines {
        // The C scans the line width up to 40 characters.
        let bytes = line.as_bytes();
        let l = bytes.len().min(40);
        // x = (vid.width - l*8)/2, an int.
        let vx = ((sc.w - l as i32 * 8) / 2) as f32;
        for (j, &c) in bytes[..l].iter().enumerate() {
            draw_char_scaled(image, conchars, vx + j as f32 * 8.0, vy, c, sc.scale, 0.0, 0.0);
            if budget == 0 {
                return; // `if (!remaining--) return;` — this char was the last.
            }
            // Post-decrement: a budget already below zero just keeps falling
            // (never re-hits the `== 0` gate), painting the whole string like
            // the C. `wrapping_sub` keeps even an `i32::MIN` caller total.
            budget = budget.wrapping_sub(1);
        }
        vy += 8.0;
    }
}

/// `SCR_DrawPause` (screen.c): while `cl.paused` (and `showpause`, default
/// 1), the `gfx/pause.lmp` plaque, centred across the 2-D screen and above
/// its middle — `Draw_Pic ((vid.width - pic->width)/2, (vid.height - 48 -
/// pic->height)/2, pic)`. (`Draw_Pic` copies every texel; the plaque has no
/// transparent ones, so the see-through blit draws the same.)
pub fn draw_pause(image: &mut Image, pic: &crate::wad::Qpic) {
    let sc = screen_2d(image.w, image.h);
    let x = (sc.w - pic.width) / 2;
    let y = (sc.h - 48 - pic.height) / 2;
    blit_qpic_at(image, pic, x as f32, y as f32, sc.scale, 0.0, 0.0);
}

/// `scr_printspeed.value * elapsed`, clamped the way a `9999`-plus budget
/// already paints anything id1 or the packs ship: the `remaining` both
/// [`crate::sbar::draw_finale_overlay`] (the draw) and
/// [`finale_text_fully_revealed`] (the mission packs' `finaleFinished`
/// builtin, below) feed from — kept in one place so neither can disagree on
/// the frame the reveal completes.
pub(crate) fn scr_printspeed_remaining(elapsed: f32) -> i32 {
    (8.0 * elapsed.max(0.0)).min(9999.0) as i32
}

/// Whether [`draw_center_string_revealed`]'s typewriter reveal has painted
/// every character of `text` by `elapsed` seconds after the finale started.
///
/// The mission packs' re-release engine exposes exactly this as a builtin,
/// `finaleFinished` (#79): `finale_check` (client.qc) polls it every 0.1s and,
/// once true, waits 5 more seconds then runs `menu_credits` +`disconnect`
/// (`server::pr_cmds::bi_finale_finished`, set each frame by
/// `client/cl_main.rs`'s `walk_frame` from exactly this client-side state —
/// `w.finale_text`/`w.finale_start`/`w.clock` — since single-player keeps
/// server and client in one process; see AUDIT.md "The mission packs'
/// paths", P7/B4). `id1`'s progs never declares the builtin, so this is dead
/// code for it.
///
/// Mirrors [`draw_center_string_revealed`]'s own math rather than redoing it:
/// a budget of `n` paints `n + 1` characters (the doc comment there), so the
/// reveal is complete once `remaining + 1` reaches the total character count
/// across every line (each truncated to 40, as the draw truncates it; `\n`
/// itself is not drawn and does not count).
pub fn finale_text_fully_revealed(text: &str, elapsed: f32) -> bool {
    let total: usize = text.split('\n').map(|line| line.len().min(40)).sum();
    if total == 0 {
        return true; // nothing to reveal
    }
    let remaining = scr_printspeed_remaining(elapsed);
    i64::from(remaining) + 1 >= total as i64
}

/// Draw a `centerprint` message: `SCR_DrawCenterString` outside the finale
/// (`remaining = 9999`, the whole string) — [`draw_center_string_revealed`].
pub fn draw_centerprint(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    text: &str,
) {
    draw_center_string_revealed(image, conchars, text, -1);
}

/// Where [`draw_fps`] puts the readout on the 2-D screen ([`screen_2d`]):
/// the top-left corner, its first cell at the notify lines' left margin
/// (`Con_DrawNotify`'s `(x+1)<<3`) on their first row (`v = 0`).
const FPS_POS: (i32, i32) = (8, 0);

/// EXTRA, not in id's Quake (Options > Classic / 2026 > Show FPS,
/// `wasm_showfps`): the frame rate as QuakeWorld's `SCR_DrawFPS`
/// (QW/client/screen.c) writes it — `sprintf(st, "%3d FPS", lastfps)` in
/// white conchars (`Draw_String`) — but in the top-left corner,
/// [`FPS_POS`], where QuakeWorld put it in the bottom-right one, just above
/// the status bar (`x = vid.width - strlen(st)*8 - 8`, `y = vid.height -
/// sb_lines - 8`): over the game, clear of the bar and the touch controls
/// above it, and where a glance finds it. The notify lines, which start in
/// that corner too, move down a row while it shows ([`notify_top`]). The
/// `%3d` keeps "FPS" still as the count changes. `fps` is the host's count
/// (QW's `lastfps`). The coordinates are the 2-D layer's screen
/// ([`screen_2d`]: the framebuffer 1:1 as id, or the "scaled 2-D" extra's).
pub fn draw_fps(image: &mut Image, conchars: &crate::wad::Qpic, fps: u32) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let sc = screen_2d(image.w, image.h);
    let st = format!("{fps:3} FPS");
    let (x, y) = FPS_POS;
    draw_string_scaled(image, conchars, x as f32, y as f32, &st, sc.scale, 0.0, 0.0);
}

/// The 2-D row the notify lines start at ([`crate::console::draw_notify`]):
/// `Con_DrawNotify`'s `v = 0`, or, while [`draw_fps`]'s readout has that
/// row's corner (`show_fps`), the text row under it — so neither covers the
/// other. Classic never shows the readout, so its notify lines are id's.
pub fn notify_top(show_fps: bool) -> i32 {
    if show_fps {
        FPS_POS.1 + 8
    } else {
        0
    }
}

/// `V_RenderView`'s crosshair (view.c, with `crosshair 1`): the conchars `+`
/// through `Draw_Character`, its cell's top-left corner at the centre of the
/// view, `(scr_vrect.x + scr_vrect.width/2, scr_vrect.y + scr_vrect.height/2)`
/// (`cl_crossx`/`cl_crossy`, id's offsets from there, are 0 and not
/// modelled). `vrect` is in framebuffer pixels; the character is drawn on the
/// 2-D layer's screen ([`screen_2d`]), so with the scaled 2-D extra it has
/// that layer's size, at the same place. The client leaves it off an
/// intermission or finale, as id's GLQuake does (`walk_frame`).
pub fn draw_crosshair(image: &mut Image, conchars: &crate::wad::Qpic, vrect: &ViewRect) {
    let sc = screen_2d(image.w, image.h);
    if image.w == 0 || image.h == 0 || sc.scale.is_nan() || sc.scale <= 0.0 {
        return;
    }
    let s = sc.scale.round().max(1.0) as usize;
    let x = (vrect.x + vrect.w / 2) / s;
    let y = (vrect.y + vrect.h / 2) / s;
    draw_char_scaled(image, conchars, x as f32, y as f32, b'+', sc.scale, 0.0, 0.0);
}

// ---------------------------------------------------------------------------
// Screen shots: SCR_ScreenShot_f, WritePCXfile
// ---------------------------------------------------------------------------

/// `SCR_ScreenShot_f`'s filename search: `quake00.pcx` .. `quake99.pcx`, the
/// first for which `exists` is false (the C's `Sys_FileTime(checkname) ==
/// -1`). `None` once all 100 are taken (the C's `i == 100` guard, "Couldn't
/// create a PCX file"), which `exists` never has to know about.
pub fn screenshot_name(exists: impl Fn(&str) -> bool) -> Option<String> {
    (0..=99).map(|i| format!("quake{i:02}.pcx")).find(|name| !exists(name))
}

/// `WritePCXfile`: `width` x `height` of 8-bit palette indices (`pixels`,
/// `width*height` of them — the port's framebuffers are never row-padded, so
/// the C's `rowbytes` is always `width` here) and the 256-colour `palette`,
/// as a type-5 (256-colour, one plane) PCX: the 128-byte header, the pixel
/// data in the C's "RLE" (a byte whose top two bits are both set is written
/// as a run of exactly one — `0xc1` then the byte itself — anything else
/// literally; the packer never actually forms a longer run), the palette
/// marker `0x0c`, then the 768-byte RGB palette.
pub fn write_pcx(width: usize, height: usize, pixels: &[u8], palette: &[[u8; 3]; 256]) -> Vec<u8> {
    let mut out = Vec::with_capacity(128 + width * height * 2 + 1 + 768);
    out.push(0x0a); // manufacturer: the PCX id
    out.push(5); // version: 256 colour
    out.push(1); // encoding: "RLE" (see above)
    out.push(8); // bits per pixel
    out.extend_from_slice(&0u16.to_le_bytes()); // xmin
    out.extend_from_slice(&0u16.to_le_bytes()); // ymin
    out.extend_from_slice(&((width - 1) as u16).to_le_bytes()); // xmax
    out.extend_from_slice(&((height - 1) as u16).to_le_bytes()); // ymax
    out.extend_from_slice(&(width as u16).to_le_bytes()); // hres
    out.extend_from_slice(&(height as u16).to_le_bytes()); // vres
    out.resize(out.len() + 48, 0); // the EGA palette, unused at 256 colours
    out.push(0); // reserved
    out.push(1); // colour planes: chunky image
    out.extend_from_slice(&(width as u16).to_le_bytes()); // bytes per line
    out.extend_from_slice(&2u16.to_le_bytes()); // palette type: not greyscale
    out.resize(out.len() + 58, 0); // filler
    debug_assert_eq!(out.len(), 128, "the PCX header is always 128 bytes");
    for &b in pixels.iter().take(width * height) {
        if b & 0xc0 == 0xc0 {
            out.push(0xc1);
        }
        out.push(b);
    }
    out.push(0x0c); // palette ID byte
    for c in palette {
        out.extend_from_slice(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::RESOLUTION_PRESETS;
    use crate::render::fixtures::{solid_conchars, solid_pic, test_backtile};
    use SbarLayout::{Classic, Overlay};

    #[test]
    fn the_pause_plaque_sits_where_scr_drawpause_puts_it() {
        // gfx/pause.lmp is 128x24: ((w - 128)/2, (h - 48 - 24)/2) in 2-D pixels.
        let pic = solid_pic(128, 24, 7);
        for (w, h, x, y, scale) in [(320, 200, 96, 64, 1), (640, 400, 256, 164, 1), (960, 600, 416, 264, 1)] {
            let mut img = Image::new(w, h, 0);
            draw_pause(&mut img, &pic);
            let lit: Vec<usize> = (0..w * h).filter(|&i| img.pixels[i] == 7).collect();
            assert_eq!(lit.len(), 128 * 24 * scale * scale, "{w}x{h}");
            assert_eq!((lit[0] % w, lit[0] / w), (x, y), "{w}x{h}: top-left");
        }
        // The scaled-2-D extra lays it out on 320x200 and blows it up.
        let _g = crate::draw::Scaled2dGuard::set(true);
        let mut img = Image::new(960, 600, 0);
        draw_pause(&mut img, &pic);
        let lit: Vec<usize> = (0..960 * 600).filter(|&i| img.pixels[i] == 7).collect();
        assert_eq!(lit.len(), 128 * 24 * 9);
        assert_eq!((lit[0] % 960, lit[0] / 960), (96 * 3, 64 * 3));
    }
    use crate::sbar::draw_finale_overlay;
    use crate::wad::Qpic;

    #[test]
    fn vid_aspect_is_vid_win_c_on_a_4_3_display() {
        // vid.aspect = ((float)h / (float)w) * (320.0 / 240.0), stored to float.
        let four_three = 4.0 / 3.0;
        for (w, h) in [(320, 200), (640, 400), (960, 600), (1280, 800)] {
            assert_eq!(vid_aspect(w, h, four_three).to_bits(), 0x3F55_5555, "{w}x{h}: 0.8333333");
        }
        assert_eq!(vid_aspect(640, 480, four_three), 1.0);
        assert_eq!(vid_aspect(320, 240, four_three), 1.0);
        // A square-pixel display of the same mode, and nonsense, are square.
        assert_eq!(vid_aspect(320, 200, 320.0 / 200.0), 1.0);
        assert_eq!(vid_aspect(0, 200, four_three), 1.0);
        assert_eq!(vid_aspect(320, 200, f64::NAN), 1.0);
    }

    #[test]
    fn centerprint_starts_at_row_69_like_x87_quake() {
        // SCR_DrawCenterString: y = vid.height*0.35, 0.35 a double a hair
        // under 0.35, the product exact in the x87's extended precision:
        // (int)69.99999999999999556 = 69 on the 320x200 screen, not 70.
        assert_eq!(center_string_top(200), 69);
        assert_eq!(center_string_top(400), 139);
        assert_eq!(center_string_top(201), 70); // 70.35
        assert_eq!(center_string_top(480), 167); // 168 exactly -> 167
        let cc = solid_conchars();
        let mut img = Image::new(320, 200, 0);
        draw_centerprint(&mut img, &cc, "AB");
        let lit = |y: usize| (0..320).any(|x| img.pixels[y * 320 + x] != 0);
        let rows: Vec<usize> = (0..200).filter(|&y| lit(y)).collect();
        assert_eq!(rows, (69..=76).collect::<Vec<_>>(), "rows 69..=76");
        // (320 - 2*8)/2 = 152: the line centers on the screen.
        assert_ne!(img.pixels[69 * 320 + 152], 0);
        assert_eq!(img.pixels[69 * 320 + 151], 0);
    }

    #[test]
    fn finale_center_string_reveals_at_printspeed() {
        // SCR_DrawCenterString's finale reveal: remaining = 8 * elapsed, and the
        // post-decrement `if (!remaining--) return;` paints remaining+1 chars.
        let cc = solid_conchars();
        // "AB\nCD": 2 lines (<= 4) so the block starts at y = 200*0.35 = 70; each
        // 2-char line centers at vx = (320 - 16)/2 = 152.
        let text = "AB\nCD";

        // elapsed 0 -> remaining 0 -> exactly ONE char ('A') is painted.
        let mut img = Image::new(320, 200, 0);
        draw_finale_overlay(&mut img, Some(&cc), None, text, 0.0);
        let px = |img: &Image, x: usize, y: usize| img.pixels[y * 320 + x];
        assert_eq!(px(&img, 152 + 1, 70 + 1), 95, "first char visible at once");
        assert_eq!(px(&img, 160 + 1, 70 + 1), 0, "second char not yet revealed");
        assert_eq!(px(&img, 152 + 1, 78 + 1), 0, "second line not yet revealed");

        // elapsed 1s -> remaining 8 -> all four chars painted (budget exceeds text).
        let mut img = Image::new(320, 200, 0);
        draw_finale_overlay(&mut img, Some(&cc), None, text, 1.0);
        assert_eq!(px(&img, 160 + 1, 70 + 1), 95, "line 1 fully revealed");
        assert_eq!(px(&img, 160 + 1, 78 + 1), 95, "line 2 fully revealed");

        // The finale plaque centers horizontally at y=16 (Sbar_FinaleOverlay).
        let plaque = Qpic { width: 100, height: 20, data: vec![52u8; 100 * 20] };
        let mut img = Image::new(320, 200, 0);
        draw_finale_overlay(&mut img, Some(&cc), Some(&plaque), "", 0.0);
        assert_eq!(px(&img, 110 + 1, 16 + 1), 52, "finale.lmp centered at y=16");
        assert_eq!(px(&img, 100, 16 + 1), 0, "left of the centered plaque is clear");
    }

    #[test]
    fn finale_text_fully_revealed_agrees_with_the_draw() {
        // "AB\nCD": 4 characters total (the '\n' doesn't count), matching
        // finale_center_string_reveals_at_printspeed above.
        let text = "AB\nCD";
        assert!(!finale_text_fully_revealed(text, 0.0), "only 'A' is painted yet");
        // 8 chars/sec: the 4th character needs remaining >= 3, i.e. elapsed >= 0.375s.
        assert!(!finale_text_fully_revealed(text, 0.374), "one tick short");
        assert!(finale_text_fully_revealed(text, 0.375), "exactly four characters' worth");
        assert!(finale_text_fully_revealed(text, 1.0), "well past the reveal");
        // A line past 40 characters truncates (as the draw does): only the
        // first 40 of each line count toward the total, needing remaining
        // >= 39 (elapsed >= 4.875s) — the 41st character never counts.
        let long = "x".repeat(41);
        assert!(finale_text_fully_revealed(&long, 39.0 / 8.0), "all 40 painted");
        assert!(!finale_text_fully_revealed(&long, 38.0 / 8.0), "39 of 40 painted");
        // Nothing to reveal: vacuously done.
        assert!(finale_text_fully_revealed("", 0.0), "empty text has nothing to wait for");
    }

    #[test]
    fn draw_fps_sits_in_the_top_left_corner_at_the_notify_margin() {
        let cc = solid_conchars();
        let lit = 95u8;
        // " 60 FPS" from (8, 0): a blank at x 8..16 (`%3d`), '6' at 16, the
        // 'S' ending at 8 + 7*8 = 64; one text row, rows 0..8, whatever the
        // status bar (the readout no longer sits on it).
        let mut img = Image::new(320, 200, 0);
        draw_fps(&mut img, &cc, 60);
        let px = |img: &Image, x: usize, y: usize| img.pixels[y * img.w + x];
        assert_eq!(px(&img, 16, 0), lit, "'6' at (16, 0)");
        assert_eq!(px(&img, 15, 0), 0, "%3d pads 60 with a blank");
        assert_eq!(px(&img, 63, 7), lit, "'S' ends at x=64");
        assert_eq!(px(&img, 64, 0), 0);
        assert_eq!(px(&img, 16, 8), 0, "one text row tall");
        let lit_rows: Vec<usize> = (0..200).filter(|&y| (0..320).any(|x| px(&img, x, y) != 0)).collect();
        assert_eq!(lit_rows, (0..8).collect::<Vec<_>>());
        // Three digits fill the pad: '1' at the margin itself.
        let mut img = Image::new(960, 600, 0);
        draw_fps(&mut img, &cc, 144);
        assert_eq!(px(&img, 8, 0), lit, "'1' at (8, 0) on a 1:1 960x600 2-D layer");
        assert_eq!(px(&img, 7, 0), 0, "an 8 px margin on the left");
        // The scaled 2-D layer blows the corner up with the rest: 3x at 960x600.
        let _g = crate::draw::Scaled2dGuard::set(true);
        let mut img = Image::new(960, 600, 0);
        draw_fps(&mut img, &cc, 144);
        assert_eq!(px(&img, 24, 0), lit, "'1' at (8, 0) x 3");
        assert_eq!(px(&img, 23, 0), 0);
        assert_eq!(px(&img, 24, 23), lit, "a 24-row cell");
        assert_eq!(px(&img, 24, 24), 0);
    }

    #[test]
    fn the_notify_lines_start_a_row_lower_while_the_readout_shows() {
        // Con_DrawNotify's v = 0, unless the readout has that row.
        assert_eq!(notify_top(false), 0);
        assert_eq!(notify_top(true), FPS_POS.1 + 8);
        // Drawn together, they never share a pixel: the readout fills row 0,
        // the first notify line row 1.
        let cc = solid_conchars();
        let mut fps = Image::new(320, 200, 0);
        draw_fps(&mut fps, &cc, 60);
        let mut notify = Image::new(320, 200, 0);
        crate::console::draw_notify(&mut notify, &cc, &["You got the shells"], notify_top(true));
        let both = (0..320 * 200).filter(|&i| fps.pixels[i] != 0 && notify.pixels[i] != 0).count();
        assert_eq!(both, 0, "the readout and the notify line overlap nowhere");
        let lit_rows: Vec<usize> = (0..200).filter(|&y| (0..320).any(|x| notify.pixels[y * 320 + x] != 0)).collect();
        assert_eq!(lit_rows, (8..16).collect::<Vec<_>>(), "the notify line on the second text row");
        // Without the readout the notify line is id's, from row 0.
        let mut id = Image::new(320, 200, 0);
        crate::console::draw_notify(&mut id, &cc, &["You got the shells"], notify_top(false));
        assert!((0..320).any(|x| id.pixels[x] != 0), "row 0");
    }

    #[test]
    fn negative_reveal_budget_paints_the_whole_string() {
        // SCR_DrawCenterString: `if (!remaining--) return;` only fires when
        // `remaining` is exactly 0 at the check — a budget that STARTS below
        // zero keeps decrementing past it and paints the WHOLE string. (An
        // early `remaining < 0 => return` would paint nothing.)
        let cc = solid_conchars();
        let px = |img: &Image, x: usize, y: usize| img.pixels[y * 320 + x];

        let mut img = Image::new(320, 200, 0);
        draw_center_string_revealed(&mut img, &cc, "AB\nCD", -1);
        assert_eq!(px(&img, 160 + 1, 70 + 1), 95, "line 1 fully painted");
        assert_eq!(px(&img, 160 + 1, 78 + 1), 95, "line 2 fully painted");

        // i32::MIN paints everything too (and the decrement must not panic).
        let mut img = Image::new(320, 200, 0);
        draw_center_string_revealed(&mut img, &cc, "AB\nCD", i32::MIN);
        assert_eq!(px(&img, 160 + 1, 78 + 1), 95, "i32::MIN = unlimited");
    }

    // -- SCR_CalcRefdef / R_SetVrect / Draw_TileClear / sb_lines -------------

    fn vr(x: usize, y: usize, w: usize, h: usize) -> ViewRect {
        ViewRect { x, y, w, h }
    }

    #[test]
    fn calc_refdef_matches_the_c_at_320x200() {
        // viewsize 100: the full width ABOVE the 48-line status bar — not a
        // full-screen view with the bar pasted over its bottom (the old bug:
        // horizon at y=100 instead of 76, 48 rows rendered only to be covered).
        let r = calc_refdef(320, 200, 100.0, false, Classic);
        assert_eq!((r.vrect, r.sb_lines), (vr(0, 0, 320, 152), 48));
        // 110: no inventory strip -> 24 lines, the view grows to 176.
        let r = calc_refdef(320, 200, 110.0, false, Classic);
        assert_eq!((r.vrect, r.sb_lines), (vr(0, 0, 320, 176), 24));
        // 120: no status bar at all -> the whole screen.
        let r = calc_refdef(320, 200, 120.0, false, Classic);
        assert_eq!((r.vrect, r.sb_lines), (vr(0, 0, 320, 200), 0));
        // 50: half size, centred horizontally on the screen and vertically in
        // the 152 rows above the bar: x = (320-160)/2, y = (152-100)/2.
        let r = calc_refdef(320, 200, 50.0, false, Classic);
        assert_eq!((r.vrect, r.sb_lines), (vr(80, 26, 160, 100), 48));
        // 30, the minimum: exactly the 96-wide "min for icons".
        let r = calc_refdef(320, 200, 30.0, false, Classic);
        assert_eq!(r.vrect, vr(112, 46, 96, 60));
        // 70: (int)(320 * 0.7f) = 224 (& ~7 = 224), (int)(200 * 0.7f) = 140.
        let r = calc_refdef(320, 200, 70.0, false, Classic);
        assert_eq!(r.vrect, vr(48, 6, 224, 140));
        // 90: 288x180 would overlap the bar -> clipped to the 152 rows above it.
        let r = calc_refdef(320, 200, 90.0, false, Classic);
        assert_eq!(r.vrect, vr(16, 0, 288, 152));
    }

    #[test]
    fn calc_refdef_bounds_viewsize_and_goes_full_screen_for_intermission() {
        // SCR_CalcRefdef clamps viewsize to 30..=120.
        assert_eq!(calc_refdef(320, 200, 5.0, false, Classic), calc_refdef(320, 200, 30.0, false, Classic));
        assert_eq!(calc_refdef(320, 200, 500.0, false, Classic), calc_refdef(320, 200, 120.0, false, Classic));
        assert_eq!(calc_refdef(320, 200, f32::NAN, false, Classic), calc_refdef(320, 200, 100.0, false, Classic));
        // "intermission is always full screen": any viewsize, no status bar.
        for vs in [30.0, 50.0, 100.0, 110.0, 120.0] {
            let r = calc_refdef(320, 200, vs, true, Classic);
            assert_eq!((r.vrect, r.sb_lines), (vr(0, 0, 320, 200), 0), "viewsize {vs}");
        }
    }

    #[test]
    fn calc_refdef_scales_the_status_bar_with_the_2d_layer() {
        // id: the status bar is 48 rows in every mode.
        assert_eq!(calc_refdef(960, 600, 100.0, false, Classic).vrect, vr(0, 0, 960, 552));
        assert_eq!(calc_refdef(480, 300, 100.0, false, Classic).vrect, vr(0, 0, 480, 252));
        assert_eq!(calc_refdef(1120, 700, 110.0, false, Classic).vrect, vr(0, 0, 1120, 676));
        assert_eq!(calc_refdef(960, 600, 50.0, false, Classic).vrect, vr(240, 126, 480, 300));
        // The "scaled 2-D" extra: the 320x200 screen scaled by the largest
        // whole number that fits, so the view clears exactly the rows
        // draw_hud_into paints: 48*scale (1.5x at 480x300 is 1x, 3.5x 3x).
        let _extra = crate::draw::Scaled2dGuard::set(true);
        assert_eq!(calc_refdef(960, 600, 100.0, false, Classic).vrect, vr(0, 0, 960, 456));
        assert_eq!(calc_refdef(480, 300, 100.0, false, Classic).vrect, vr(0, 0, 480, 252));
        assert_eq!(calc_refdef(1120, 700, 110.0, false, Classic).vrect, vr(0, 0, 1120, 628));
        assert_eq!(calc_refdef(1280, 800, 120.0, false, Classic).vrect, vr(0, 0, 1280, 800));
        // 960x600 at 50: 480x300 centred above the 144-row bar.
        assert_eq!(calc_refdef(960, 600, 50.0, false, Classic).vrect, vr(240, 78, 480, 300));
        // Every preset at every step stays inside the frame and above the bar.
        for &(w, h) in RESOLUTION_PRESETS.iter() {
            for step in 3..=12 {
                let r = calc_refdef(w as usize, h as usize, step as f32 * 10.0, false, Classic);
                let bar = (r.sb_lines as f32 * crate::draw::screen_2d(w as usize, h as usize).scale).ceil() as usize;
                assert!(r.vrect.x + r.vrect.w <= w as usize);
                assert!(r.vrect.y + r.vrect.h + bar <= h as usize, "{w}x{h} @ {step}0");
                assert_eq!(r.vrect.w % 8, 0);
                assert_eq!(r.vrect.h % 2, 0);
            }
        }
        // A degenerate frame never panics or escapes the bounds.
        let r = calc_refdef(8, 4, 30.0, false, Classic);
        assert!(r.vrect.x + r.vrect.w <= 8 && r.vrect.y + r.vrect.h <= 4);
        let _ = calc_refdef(0, 0, 100.0, false, Classic);
    }

    #[test]
    fn status_bar_rows_is_calc_refdefs_bar_in_framebuffer_pixels() {
        // id (no "scaled 2-D"): the bar is exactly sb_lines pixels, in every mode.
        assert_eq!(status_bar_rows(960, 600, 100.0, false), 48);
        assert_eq!(status_bar_rows(960, 600, 110.0, false), 24);
        assert_eq!(status_bar_rows(960, 600, 120.0, false), 0);
        // An intermission is always full screen, whatever viewsize was.
        assert_eq!(status_bar_rows(960, 600, 100.0, true), 0);
        // The "scaled 2-D" extra blows the bar up with the rest of the 2-D
        // layer: 48 virtual rows at the largest whole scale that fits.
        let _extra = crate::draw::Scaled2dGuard::set(true);
        assert_eq!(status_bar_rows(960, 600, 100.0, false), 144, "48 rows at 3x (960/320 = 3)");
        // Agrees with calc_refdef's own bar arithmetic (the test above) at
        // every preset and Screen size.
        for &(w, h) in RESOLUTION_PRESETS.iter() {
            for step in 3..=12 {
                let viewsize = step as f32 * 10.0;
                let r = calc_refdef(w as usize, h as usize, viewsize, false, Classic);
                let bar = (r.sb_lines as f32 * crate::draw::screen_2d(w as usize, h as usize).scale).ceil() as i64;
                assert_eq!(status_bar_rows(w as usize, h as usize, viewsize, false), bar, "{w}x{h} @ {viewsize}");
            }
        }
    }

    #[test]
    fn the_bar_over_the_view_gives_the_view_the_whole_frame() {
        // SbarLayout::Overlay: at viewsize 100 and 110 the view is the whole
        // frame, as at 120; sb_lines is unchanged, so the bar is still drawn
        // and the touch controls still keep clear of the same rows.
        for (vs, sb_lines) in [(100.0, 48), (110.0, 24), (120.0, 0)] {
            let r = calc_refdef(320, 200, vs, false, Overlay);
            assert_eq!((r.vrect, r.sb_lines), (vr(0, 0, 320, 200), sb_lines), "viewsize {vs}");
            assert_eq!(r.sb_lines, calc_refdef(320, 200, vs, false, Classic).sb_lines);
        }
        // Below 100 viewsize still scales the view, now centred on the whole
        // screen (QuakeSpasm's scr_sbaralpha layout): 50 is 160x100 at
        // ((320-160)/2, (200-100)/2); at 90 (288x180) the bar's 48 rows cover
        // the view's bottom 38 and the border's 10.
        assert_eq!(calc_refdef(320, 200, 50.0, false, Overlay).vrect, vr(80, 50, 160, 100));
        assert_eq!(calc_refdef(320, 200, 90.0, false, Overlay).vrect, vr(16, 10, 288, 180));
        assert_eq!(calc_refdef(320, 200, 30.0, false, Overlay).vrect, vr(112, 70, 96, 60));
        // An intermission is full screen in either layout.
        assert_eq!(calc_refdef(320, 200, 100.0, true, Overlay), calc_refdef(320, 200, 100.0, true, Classic));
        // The 2026 frames: 1920x1080 at pixel size 1 (the 2-D layer at 5x,
        // the bar 240 rows) and a wide frame (1315x535, 2x, 96 rows). id's
        // layout keeps the view above the bar; over it the view is the frame
        // (its width a multiple of 8 and its height even, R_SetVrect's: 1312
        // at x 1 and 534 rows in the wide frame), and its centre — the
        // crosshair's — moves down by half the bar's rows.
        let _extra = crate::draw::Scaled2dGuard::set(true);
        for (w, h, bar, x, view_w, id_h, over_h) in [(1920, 1080, 240, 0, 1920, 840, 1080), (1315, 535, 96, 1, 1312, 438, 534)] {
            let id = calc_refdef(w, h, 100.0, false, Classic);
            let over = calc_refdef(w, h, 100.0, false, Overlay);
            assert_eq!(status_bar_rows(w, h, 100.0, false), bar as i64, "{w}x{h}: the bar's rows");
            assert_eq!(id.vrect, vr(x, 0, view_w, id_h), "{w}x{h}: id's view above the bar");
            assert_eq!(over.vrect, vr(x, 0, view_w, over_h), "{w}x{h}: the view is the whole frame");
            assert_eq!(over.sb_lines, id.sb_lines);
            let centre_y = |r: ViewRect| r.y + r.h / 2;
            assert_eq!(centre_y(over.vrect) - centre_y(id.vrect), bar / 2, "{w}x{h}: the centre moves down half the bar");
        }
        // Every preset at every step stays inside the frame.
        for &(w, h) in RESOLUTION_PRESETS.iter() {
            for step in 3..=12 {
                let r = calc_refdef(w as usize, h as usize, step as f32 * 10.0, false, Overlay);
                assert!(r.vrect.x + r.vrect.w <= w as usize && r.vrect.y + r.vrect.h <= h as usize);
                if step >= 10 {
                    assert_eq!(r.vrect, vr(0, 0, w as usize, h as usize), "{w}x{h} @ {step}0: the whole frame");
                }
            }
        }
    }

    #[test]
    fn the_warp_buffer_follows_the_bar_layout() {
        // Underwater, the warp buffer's view is the screen's own: with the
        // bar over the view, the whole 320x200 buffer (id's at viewsize 120).
        assert_eq!(warp_vrect(960, 600, 100.0, false, Overlay, false), vr(0, 0, 320, 200));
        assert_eq!(warp_vrect(320, 200, 100.0, false, Overlay, false), calc_refdef(320, 200, 100.0, false, Overlay).vrect);
        assert_eq!(warp_vrect(960, 600, 50.0, false, Overlay, false), vr(80, 50, 160, 100));
        // hires: the view rectangle itself.
        for vs in [50.0, 100.0, 110.0] {
            assert_eq!(warp_vrect(1920, 1080, vs, false, Overlay, true), calc_refdef(1920, 1080, vs, false, Overlay).vrect);
        }
    }

    #[test]
    fn warp_vrect_is_r_setupframes_warp_buffer_view() {
        // No larger than 320x200: the screen's own view rectangle (1:1 warp).
        for vs in [30.0, 50.0, 100.0, 110.0, 120.0] {
            assert_eq!(warp_vrect(320, 200, vs, false, Classic, false), calc_refdef(320, 200, vs, false, Classic).vrect);
        }
        // id's 48-row bar on a 960x600 screen is (int)(48 * 200/600) = 16 rows
        // of the warp buffer.
        assert_eq!(warp_vrect(960, 600, 100.0, false, Classic, false), vr(0, 0, 320, 184));
        assert_eq!(warp_vrect(640, 400, 100.0, false, Classic, false), vr(0, 0, 320, 176));
        // The "scaled 2-D" extra's bar is 48 rows of the 320x200 screen.
        let _extra = crate::draw::Scaled2dGuard::set(true);
        // Every 16:10 preset the extra draws at a whole multiple of 320x200
        // renders underwater into the 320x200 screen's view rectangle (the
        // status-bar lines scaled back by h / vid.height) — which D_WarpScreen
        // stretches over the preset's own view rectangle.
        for &(w, h) in RESOLUTION_PRESETS.iter().filter(|&&(w, _)| w % 320 == 0) {
            for step in 3..=12 {
                let vs = step as f32 * 10.0;
                let want = calc_refdef(320, 200, vs, false, Classic).vrect;
                assert_eq!(warp_vrect(w as usize, h as usize, vs, false, Classic, false), want, "{w}x{h} @ {vs}");
            }
            assert_eq!(warp_vrect(w as usize, h as usize, 50.0, true, Classic, false), vr(0, 0, 320, 200));
        }
        assert_eq!(warp_vrect(960, 600, 50.0, false, Classic, false), vr(80, 26, 160, 100));
        // Wider than 16:10: 320 wide, the height follows the mode (C's too).
        assert_eq!(warp_vrect(1280, 600, 120.0, false, Classic, false), vr(0, 0, 320, 150));
        // Taller (4:3): id squeezes 320x200 with its pixel aspect; the square-
        // pixel port keeps the shape instead (266 wide, &~7 -> 264).
        assert_eq!(warp_vrect(640, 480, 120.0, false, Classic, false), vr(1, 0, 264, 200));
    }

    #[test]
    fn hires_renders_underwater_at_the_view_itself() {
        // The hires extra has no warp buffer: the view rectangle, at any size.
        for (w, h) in [(320, 200), (960, 600), (1920, 1080), (3840, 2160)] {
            for vs in [50.0, 100.0, 120.0] {
                assert_eq!(warp_vrect(w, h, vs, false, Classic, true), calc_refdef(w, h, vs, false, Classic).vrect, "{w}x{h} @ {vs}");
            }
        }
    }

    #[test]
    fn compose_view_places_the_view_inside_a_backtile_border() {
        let tile = test_backtile();
        // viewsize 50 at 320x200: a 160x100 view at (80, 26).
        let r = calc_refdef(320, 200, 50.0, false, Classic);
        let view = Image::new(r.vrect.w, r.vrect.h, 250);
        let img = compose_view(view, r.vrect, 320, 200, Some(&tile), 1);
        assert_eq!((img.w, img.h), (320, 200));
        let tile_at = |x: usize, y: usize| tile.data[(y % 64) * 64 + x % 64];
        for y in 0..200 {
            for x in 0..320 {
                let inside = (80..240).contains(&x) && (26..126).contains(&y);
                let want = if inside { 250 } else { tile_at(x, y) };
                assert_eq!(img.pixels[y * 320 + x], want, "({x},{y})");
            }
        }
        // A full-screen view (viewsize 120) passes through untouched.
        let full = calc_refdef(320, 200, 120.0, false, Classic);
        let view = Image::new(320, 200, 250);
        let out = compose_view(view, full.vrect, 320, 200, Some(&tile), 1);
        assert!(out.pixels.iter().all(|&p| p == 250));
    }

    #[test]
    fn compose_view_on_a_dirty_spare_buffer_matches_a_fresh_full_clear() {
        // The screen is a recycled buffer left uncleared, so every pixel must
        // be written: compare with the straightforward compose (a fresh
        // screen, the tile everywhere, the view copied over it) with garbage
        // in every spare buffer first, for every viewsize, a few sizes, with
        // and without a backtile, and views hanging off the screen.
        let tile = test_backtile();
        let reference = |view: &Image, vrect: ViewRect, w: usize, h: usize, t: Option<&Qpic>| {
            let mut img = Image::new(w, h, 0);
            draw_tile_clear(&mut img, t, 0, 0, w, h);
            for vy in 0..view.h {
                for vx in 0..view.w {
                    img.put((vrect.x + vx) as i32, (vrect.y + vy) as i32, view.pixels[vy * view.w + vx]);
                }
            }
            img.pixels
        };
        let mut cases = Vec::new();
        for &(w, h) in &[(320, 200), (640, 400), (1280, 800), (400, 300)] {
            for vs in (30..=120).step_by(10) {
                cases.push((w, h, calc_refdef(w, h, vs as f32, false, Classic).vrect));
            }
        }
        cases.push((320, 200, ViewRect { x: 300, y: 190, w: 64, h: 32 }));
        cases.push((320, 200, ViewRect { x: 400, y: 10, w: 16, h: 16 }));
        cases.push((320, 200, ViewRect { x: 0, y: 0, w: 100, h: 300 }));
        for (i, &(w, h, vrect)) in cases.iter().enumerate() {
            let view = Image::new(vrect.w, vrect.h, 250 - (i % 7) as u8);
            for t in [Some(&tile), None] {
                for _ in 0..3 {
                    crate::render::recycle_image(Image::new(w, h + 7, 1));
                }
                let want = reference(&view, vrect, w, h, t);
                let got = compose_view(
                    Image { w: view.w, h: view.h, pixels: view.pixels.clone() },
                    vrect,
                    w,
                    h,
                    t,
                    3,
                );
                assert_eq!((got.w, got.h), (w, h));
                assert!(got.pixels == want, "{w}x{h} {vrect:?} tile {}", t.is_some());
            }
        }
    }

    #[test]
    fn screenshot_name_finds_the_first_free_slot_and_gives_up_at_100() {
        assert_eq!(screenshot_name(|_| false), Some("quake00.pcx".to_string()), "a fresh directory");
        let taken = ["quake00.pcx", "quake01.pcx", "quake02.pcx"];
        assert_eq!(screenshot_name(|n| taken.contains(&n)), Some("quake03.pcx".to_string()));
        assert_eq!(screenshot_name(|_| true), None, "every one of the 100 is taken");
    }

    #[test]
    fn write_pcx_is_a_128_byte_header_the_rle_lite_pixels_then_the_palette() {
        let palette: [[u8; 3]; 256] = std::array::from_fn(|i| [i as u8, (2 * i) as u8, (3 * i) as u8]);
        // 0x05 is a literal byte; 0xc3 and 0xff have their top two bits set, so
        // each becomes the C's "run of one": 0xc1 then the byte itself.
        let pixels = [0x05, 0xc3, 0x00, 0xff];
        let out = write_pcx(4, 1, &pixels, &palette);
        // Header: manufacturer, version, encoding, bits-per-pixel, then the
        // bounds (xmax = width-1, ymax = height-1) and resolution, little-endian.
        assert_eq!(out[0..4], [0x0a, 5, 1, 8]);
        assert_eq!(&out[8..10], &3u16.to_le_bytes(), "xmax = width - 1");
        assert_eq!(&out[10..12], &0u16.to_le_bytes(), "ymax = height - 1");
        assert_eq!(&out[12..14], &4u16.to_le_bytes(), "hres = width");
        assert_eq!(&out[66..68], &4u16.to_le_bytes(), "bytes per line = width");
        assert_eq!(out[65], 1, "one colour plane");

        let pixels_out = &out[128..];
        // 0x05 literal; 0xc3 as 0xc1,0xc3; 0x00 literal; 0xff as 0xc1,0xff;
        // then the palette marker and the 768-byte palette.
        assert_eq!(pixels_out[..9], [0x05, 0xc1, 0xc3, 0x00, 0xc1, 0xff, 0x0c, 0, 0]);
        assert_eq!(out.len(), 128 + 6 + 1 + 768, "header + packed pixels + marker + palette");
        let pal_bytes = &out[out.len() - 768..];
        for (i, c) in palette.iter().enumerate() {
            assert_eq!(&pal_bytes[i * 3..i * 3 + 3], c, "palette entry {i}");
        }

        // A run of ordinary bytes never gets RLE-expanded: id's packer only
        // ever emits a "run" of exactly one, never combines repeats.
        let plain = [1u8, 2, 3, 4, 5, 6];
        let out = write_pcx(6, 1, &plain, &palette);
        assert_eq!(&out[128..134], &plain[..]);
        assert_eq!(out.len(), 128 + 6 + 1 + 768);
    }
}
