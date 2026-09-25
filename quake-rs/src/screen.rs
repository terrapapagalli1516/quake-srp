//! Screen layout: the view rectangle, the composed frame, centre prints.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/screen.c` — `SCR_CalcRefdef` (with `R_SetVrect`, `r_main.c`),
//! `SCR_UpdateScreen`'s tile-cleared border, `SCR_DrawCenterString`.

use crate::draw::{
    draw_char_scaled, draw_string_scaled, draw_tile_clear, HUD_VIRT_W, MENU_VIRT_H, MENU_VIRT_W,
};
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
/// is a 4:3 monitor (`display_aspect` 4/3). It is the height of a displayed
/// pixel over its width, the `pixelAspect` of `R_ViewChanged`
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
    /// `sb_lines` in the status bar's 320x200 virtual rows: 48 (sbar + inventory),
    /// 24 (sbar only) or 0 (no status bar).
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
/// The one adaptation: this port draws the 2-D layer (status bar, menus) as the
/// 320x200 virtual screen scaled by `vid_w/320`, so the status bar the view must
/// clear (`lineadj`) is `sb_lines` scaled to framebuffer rows — exactly the
/// rows [`draw_hud_into`](crate::sbar::draw_hud_into) paints. At 320x200 every number is the C's.
///
/// The arithmetic keeps the C's types: `size` is a `float`, the products are
/// truncated to `int` (so e.g. 70% of 320 is `(int)(320 * 0.7f) = 224`, as an
/// IEEE-single build computes it).
pub fn calc_refdef(vid_w: usize, vid_h: usize, viewsize: f32, intermission: bool) -> Refdef {
    let (viewsize, sb_lines, lineadj) = status_lines(vid_w, vid_h, viewsize, intermission);
    Refdef { vrect: set_vrect(vid_w as i64, vid_h as i64, viewsize, lineadj, intermission), sb_lines }
}

/// SCR_CalcRefdef's first half: the bounded `viewsize` (a non-number reads as
/// the default), `sb_lines`, and the framebuffer rows the status bar covers
/// (the `lineadj` R_SetVrect keeps the view above: `sb_lines` scaled like the
/// 2-D layer, see [`calc_refdef`]).
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
    // The status bar's framebuffer rows: draw_hud_into scales the 320-wide bar
    // by vid_w/320 and bottom-anchors it, so it covers ceil(sb_lines * scale).
    let scale = vid_w as f32 / HUD_VIRT_W;
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
/// (here the port's scaled `lineadj`, the C's unscaled sbar). At every 16:10
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
pub fn warp_vrect(vid_w: usize, vid_h: usize, viewsize: f32, intermission: bool) -> ViewRect {
    let (viewsize, _, lineadj) = status_lines(vid_w, vid_h, viewsize, intermission);
    if vid_w <= WARP_WIDTH && vid_h <= WARP_HEIGHT {
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

/// Put the rendered 3-D `view` (a `vrect.w x vrect.h` image) into a
/// `vid_w x vid_h` screen at `vrect`, with everything outside it tile-cleared
/// ([`draw_tile_clear`] — SCR_UpdateScreen's `Draw_TileClear(0,0,vid.width,
/// vid.height)` under the view). A view that already IS the whole screen
/// (viewsize 120, an intermission) comes back untouched, at zero cost. The
/// status bar is drawn over the result afterwards, as in the C.
///
/// Every screen pixel is written exactly once — the tile only goes where the
/// view does not, the four bands around it — so the screen is a spare frame
/// buffer left uncleared ([`Image::reused_uncleared`]), and the view's own
/// buffer goes back to the pool ([`crate::render::recycle_image`]).
pub fn compose_view(
    view: Image,
    vrect: ViewRect,
    vid_w: usize,
    vid_h: usize,
    backtile: Option<&crate::wad::Qpic>,
    palette: &[[u8; 3]; 256],
) -> Image {
    if vrect.x == 0 && vrect.y == 0 && view.w == vid_w && view.h == vid_h {
        return view;
    }
    let mut img = Image::reused_uncleared(vid_w, vid_h);
    // The rectangle the view covers, clipped to the screen: [x0, x1) x [y0, y1).
    let x0 = vrect.x.min(vid_w);
    let x1 = x0 + view.w.min(vid_w - x0);
    let y0 = vrect.y.min(vid_h);
    let y1 = y0 + view.h.min(vid_h - y0);
    // The tile everywhere else: above, below, then either side.
    draw_tile_clear(&mut img, backtile, 0, 0, vid_w, y0, palette);
    draw_tile_clear(&mut img, backtile, 0, y1, vid_w, vid_h - y1, palette);
    draw_tile_clear(&mut img, backtile, 0, y0, x0, y1 - y0, palette);
    draw_tile_clear(&mut img, backtile, x1, y0, vid_w - x1, y1 - y0, palette);
    let cw = x1 - x0;
    for (vy, py) in (y0..y1).enumerate() {
        let dst = py * vid_w + x0;
        img.rgb[dst..dst + cw].copy_from_slice(&view.rgb[vy * view.w..vy * view.w + cw]);
    }
    crate::render::recycle_image(view);
    img
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
    palette: &[[u8; 3]; 256],
    text: &str,
    remaining: i32,
) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let sx = image.w as f32 / MENU_VIRT_W;
    let sy = image.h as f32 / MENU_VIRT_H;
    let scale = sx.min(sy);
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    let ox = (image.w as f32 - MENU_VIRT_W * scale) * 0.5;
    let oy = (image.h as f32 - MENU_VIRT_H * scale) * 0.5;

    let lines: Vec<&str> = text.split('\n').collect();
    // scr_center_lines <= 4 => y = vid.height*0.35 (virtual 70); taller => 48.
    let mut vy = if lines.len() <= 4 { 200.0 * 0.35 } else { 48.0 };
    let mut budget = remaining;
    for line in lines {
        // The C scans the line width up to 40 characters.
        let bytes = line.as_bytes();
        let l = bytes.len().min(40);
        let vx = (MENU_VIRT_W - l as f32 * 8.0) * 0.5;
        for (j, &c) in bytes[..l].iter().enumerate() {
            draw_char_scaled(image, conchars, vx + j as f32 * 8.0, vy, c, scale, ox, oy, palette);
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

/// Draw a `centerprint` message (SCR_DrawCenterString): each '\n'-split line is
/// centered horizontally in the 320x200 virtual screen and the block is centered
/// vertically, scaled to the framebuffer (`image.w/320`, the HUD/menu scale).
/// No-op without conchars or on an empty frame.
pub fn draw_centerprint(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    palette: &[[u8; 3]; 256],
    text: &str,
) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let scale = image.w as f32 / HUD_VIRT_W;
    let lines: Vec<&str> = text.split('\n').collect();
    // SCR_DrawCenterString: short messages (<= 4 lines) sit in the upper third at
    // y = vid.height*0.35 (200*0.35 = 70 in the virtual screen); taller blocks
    // start at y = 48 so they don't run off the bottom. NOT dead-centre.
    let mut vy = if lines.len() <= 4 { 200.0 * 0.35 } else { 48.0 };
    for line in lines {
        let w = line.len() as f32 * 8.0;
        let vx = ((HUD_VIRT_W - w) * 0.5).max(0.0);
        draw_string_scaled(image, conchars, vx, vy, line, scale, 0.0, 0.0, palette);
        vy += 8.0;
    }
}

/// EXTRA, not in id's Quake (Options > Web extras, `wasm_showfps`): the frame
/// rate as QuakeWorld's `SCR_DrawFPS` (QW/client/screen.c) draws it —
/// `sprintf(st, "%3d FPS", lastfps)` in white conchars (`Draw_String`) at
/// `x = vid.width - strlen(st)*8 - 8`, `y = vid.height - sb_lines - 8`: the
/// bottom-right corner, just above the status bar. `fps` is the host's
/// count (QW's `lastfps`). The coordinates are the port's 2-D layer: the
/// 320-wide virtual screen scaled by `image.w/320` and anchored to the
/// bottom, like the status bar ([`draw_hud_into`](crate::sbar::draw_hud_into)).
pub fn draw_fps(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    palette: &[[u8; 3]; 256],
    fps: u32,
    sb_lines: i32,
) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let scale = image.w as f32 / HUD_VIRT_W;
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    let st = format!("{fps:3} FPS");
    let vx = HUD_VIRT_W - st.len() as f32 * 8.0 - 8.0;
    let vid_h = image.h as f32 / scale;
    let vy = vid_h - sb_lines.max(0) as f32 - 8.0;
    draw_string_scaled(image, conchars, vx, vy, &st, scale, 0.0, 0.0, palette);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::RESOLUTION_PRESETS;
    use crate::render::fixtures::{ramp_palette, solid_conchars, test_backtile};
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
    fn finale_center_string_reveals_at_printspeed() {
        // SCR_DrawCenterString's finale reveal: remaining = 8 * elapsed, and the
        // post-decrement `if (!remaining--) return;` paints remaining+1 chars.
        let pal = ramp_palette();
        let cc = solid_conchars();
        // "AB\nCD": 2 lines (<= 4) so the block starts at y = 200*0.35 = 70; each
        // 2-char line centers at vx = (320 - 16)/2 = 152.
        let text = "AB\nCD";

        // elapsed 0 -> remaining 0 -> exactly ONE char ('A') is painted.
        let mut img = Image::new(320, 200, [0, 0, 0]);
        draw_finale_overlay(&mut img, Some(&cc), &pal, None, text, 0.0);
        let px = |img: &Image, x: usize, y: usize| img.rgb[y * 320 + x];
        assert_eq!(px(&img, 152 + 1, 70 + 1), [95, 95, 95], "first char visible at once");
        assert_eq!(px(&img, 160 + 1, 70 + 1), [0, 0, 0], "second char not yet revealed");
        assert_eq!(px(&img, 152 + 1, 78 + 1), [0, 0, 0], "second line not yet revealed");

        // elapsed 1s -> remaining 8 -> all four chars painted (budget exceeds text).
        let mut img = Image::new(320, 200, [0, 0, 0]);
        draw_finale_overlay(&mut img, Some(&cc), &pal, None, text, 1.0);
        assert_eq!(px(&img, 160 + 1, 70 + 1), [95, 95, 95], "line 1 fully revealed");
        assert_eq!(px(&img, 160 + 1, 78 + 1), [95, 95, 95], "line 2 fully revealed");

        // The finale plaque centers horizontally at y=16 (Sbar_FinaleOverlay).
        let plaque = Qpic { width: 100, height: 20, data: vec![52u8; 100 * 20] };
        let mut img = Image::new(320, 200, [0, 0, 0]);
        draw_finale_overlay(&mut img, Some(&cc), &pal, Some(&plaque), "", 0.0);
        assert_eq!(px(&img, 110 + 1, 16 + 1), [52, 52, 52], "finale.lmp centered at y=16");
        assert_eq!(px(&img, 100, 16 + 1), [0, 0, 0], "left of the centered plaque is clear");
    }

    #[test]
    fn draw_fps_sits_bottom_right_above_the_status_bar_like_scr_drawfps() {
        let pal = ramp_palette();
        let cc = solid_conchars();
        let lit = pal[95];
        // " 60 FPS": x = 320 - 7*8 - 8 = 256 (a blank), '6' at 264; y = 200 -
        // sb_lines - 8.
        for (sb_lines, y) in [(48, 144usize), (24, 168), (0, 192)] {
            let mut img = Image::new(320, 200, [0, 0, 0]);
            draw_fps(&mut img, &cc, &pal, 60, sb_lines);
            let px = |x: usize, y: usize| img.rgb[y * 320 + x];
            assert_eq!(px(264, y), lit, "sb_lines {sb_lines}: '6' at (264, {y})");
            assert_eq!(px(311, y + 7), lit, "'S' ends at x=311 (8 px from the edge)");
            assert_eq!(px(312, y), [0, 0, 0], "an 8 px margin on the right");
            assert_eq!(px(263, y), [0, 0, 0], "%3d pads 60 with a blank");
            assert_eq!(px(264, y - 1), [0, 0, 0], "one text row tall");
            if y + 8 < 200 {
                assert_eq!(px(264, y + 8), [0, 0, 0], "one text row tall");
            }
        }
        // Three digits fill the pad; the 2-D layer scales with the frame
        // (960x600: scale 3, the bar bottom-anchored).
        let mut img = Image::new(960, 600, [0, 0, 0]);
        draw_fps(&mut img, &cc, &pal, 144, 48);
        let px = |x: usize, y: usize| img.rgb[y * 960 + x];
        assert_eq!(px(256 * 3, 144 * 3), lit, "'1' at virtual (256, 144)");
        assert_eq!(px(256 * 3 - 1, 144 * 3), [0, 0, 0]);
    }

    #[test]
    fn negative_reveal_budget_paints_the_whole_string() {
        // SCR_DrawCenterString: `if (!remaining--) return;` only fires when
        // `remaining` is exactly 0 at the check — a budget that STARTS below
        // zero keeps decrementing past it and paints the WHOLE string. (An
        // early `remaining < 0 => return` would paint nothing.)
        let pal = ramp_palette();
        let cc = solid_conchars();
        let px = |img: &Image, x: usize, y: usize| img.rgb[y * 320 + x];

        let mut img = Image::new(320, 200, [0, 0, 0]);
        draw_center_string_revealed(&mut img, &cc, &pal, "AB\nCD", -1);
        assert_eq!(px(&img, 160 + 1, 70 + 1), [95, 95, 95], "line 1 fully painted");
        assert_eq!(px(&img, 160 + 1, 78 + 1), [95, 95, 95], "line 2 fully painted");

        // i32::MIN paints everything too (and the decrement must not panic).
        let mut img = Image::new(320, 200, [0, 0, 0]);
        draw_center_string_revealed(&mut img, &cc, &pal, "AB\nCD", i32::MIN);
        assert_eq!(px(&img, 160 + 1, 78 + 1), [95, 95, 95], "i32::MIN = unlimited");
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
        let r = calc_refdef(320, 200, 100.0, false);
        assert_eq!((r.vrect, r.sb_lines), (vr(0, 0, 320, 152), 48));
        // 110: no inventory strip -> 24 lines, the view grows to 176.
        let r = calc_refdef(320, 200, 110.0, false);
        assert_eq!((r.vrect, r.sb_lines), (vr(0, 0, 320, 176), 24));
        // 120: no status bar at all -> the whole screen.
        let r = calc_refdef(320, 200, 120.0, false);
        assert_eq!((r.vrect, r.sb_lines), (vr(0, 0, 320, 200), 0));
        // 50: half size, centred horizontally on the screen and vertically in
        // the 152 rows above the bar: x = (320-160)/2, y = (152-100)/2.
        let r = calc_refdef(320, 200, 50.0, false);
        assert_eq!((r.vrect, r.sb_lines), (vr(80, 26, 160, 100), 48));
        // 30, the minimum: exactly the 96-wide "min for icons".
        let r = calc_refdef(320, 200, 30.0, false);
        assert_eq!(r.vrect, vr(112, 46, 96, 60));
        // 70: (int)(320 * 0.7f) = 224 (& ~7 = 224), (int)(200 * 0.7f) = 140.
        let r = calc_refdef(320, 200, 70.0, false);
        assert_eq!(r.vrect, vr(48, 6, 224, 140));
        // 90: 288x180 would overlap the bar -> clipped to the 152 rows above it.
        let r = calc_refdef(320, 200, 90.0, false);
        assert_eq!(r.vrect, vr(16, 0, 288, 152));
    }

    #[test]
    fn calc_refdef_bounds_viewsize_and_goes_full_screen_for_intermission() {
        // SCR_CalcRefdef clamps viewsize to 30..=120.
        assert_eq!(calc_refdef(320, 200, 5.0, false), calc_refdef(320, 200, 30.0, false));
        assert_eq!(calc_refdef(320, 200, 500.0, false), calc_refdef(320, 200, 120.0, false));
        assert_eq!(calc_refdef(320, 200, f32::NAN, false), calc_refdef(320, 200, 100.0, false));
        // "intermission is always full screen": any viewsize, no status bar.
        for vs in [30.0, 50.0, 100.0, 110.0, 120.0] {
            let r = calc_refdef(320, 200, vs, true);
            assert_eq!((r.vrect, r.sb_lines), (vr(0, 0, 320, 200), 0), "viewsize {vs}");
        }
    }

    #[test]
    fn calc_refdef_scales_the_status_bar_with_the_2d_layer() {
        // The port's 2-D layer is the 320x200 screen scaled by w/320, so the
        // view clears exactly the rows draw_hud_into paints: 48*scale.
        assert_eq!(calc_refdef(960, 600, 100.0, false).vrect, vr(0, 0, 960, 456));
        assert_eq!(calc_refdef(480, 300, 100.0, false).vrect, vr(0, 0, 480, 228));
        assert_eq!(calc_refdef(1120, 700, 110.0, false).vrect, vr(0, 0, 1120, 616));
        assert_eq!(calc_refdef(1280, 800, 120.0, false).vrect, vr(0, 0, 1280, 800));
        // 960x600 at 50: 480x300 centred above the 144-row bar.
        assert_eq!(calc_refdef(960, 600, 50.0, false).vrect, vr(240, 78, 480, 300));
        // Every preset at every step stays inside the frame and above the bar.
        for &(w, h) in RESOLUTION_PRESETS.iter() {
            for step in 3..=12 {
                let r = calc_refdef(w as usize, h as usize, step as f32 * 10.0, false);
                let bar = (r.sb_lines as f32 * w as f32 / 320.0).ceil() as usize;
                assert!(r.vrect.x + r.vrect.w <= w as usize);
                assert!(r.vrect.y + r.vrect.h + bar <= h as usize, "{w}x{h} @ {step}0");
                assert_eq!(r.vrect.w % 8, 0);
                assert_eq!(r.vrect.h % 2, 0);
            }
        }
        // A degenerate frame never panics or escapes the bounds.
        let r = calc_refdef(8, 4, 30.0, false);
        assert!(r.vrect.x + r.vrect.w <= 8 && r.vrect.y + r.vrect.h <= 4);
        let _ = calc_refdef(0, 0, 100.0, false);
    }

    #[test]
    fn warp_vrect_is_r_setupframes_warp_buffer_view() {
        // No larger than 320x200: the screen's own view rectangle (1:1 warp).
        for vs in [30.0, 50.0, 100.0, 110.0, 120.0] {
            assert_eq!(warp_vrect(320, 200, vs, false), calc_refdef(320, 200, vs, false).vrect);
        }
        // Every 16:10 preset renders underwater into the 320x200 screen's view
        // rectangle (the status-bar lines scaled back by h / vid.height) —
        // which D_WarpScreen stretches over the preset's own view rectangle.
        for &(w, h) in RESOLUTION_PRESETS.iter() {
            for step in 3..=12 {
                let vs = step as f32 * 10.0;
                let want = calc_refdef(320, 200, vs, false).vrect;
                assert_eq!(warp_vrect(w as usize, h as usize, vs, false), want, "{w}x{h} @ {vs}");
            }
            assert_eq!(warp_vrect(w as usize, h as usize, 50.0, true), vr(0, 0, 320, 200));
        }
        assert_eq!(warp_vrect(960, 600, 50.0, false), vr(80, 26, 160, 100));
        // Wider than 16:10: 320 wide, the height follows the mode (C's too).
        assert_eq!(warp_vrect(1280, 600, 120.0, false), vr(0, 0, 320, 150));
        // Taller (4:3): id squeezes 320x200 with its pixel aspect; the square-
        // pixel port keeps the shape instead (266 wide, &~7 -> 264).
        assert_eq!(warp_vrect(640, 480, 120.0, false), vr(1, 0, 264, 200));
    }

    #[test]
    fn compose_view_places_the_view_inside_a_backtile_border() {
        let pal = ramp_palette();
        let tile = test_backtile();
        // viewsize 50 at 320x200: a 160x100 view at (80, 26).
        let r = calc_refdef(320, 200, 50.0, false);
        let view = Image::new(r.vrect.w, r.vrect.h, [250, 1, 2]);
        let img = compose_view(view, r.vrect, 320, 200, Some(&tile), &pal);
        assert_eq!((img.w, img.h), (320, 200));
        let tile_at = |x: usize, y: usize| pal[tile.data[(y % 64) * 64 + x % 64] as usize];
        for y in 0..200 {
            for x in 0..320 {
                let inside = (80..240).contains(&x) && (26..126).contains(&y);
                let want = if inside { [250, 1, 2] } else { tile_at(x, y) };
                assert_eq!(img.rgb[y * 320 + x], want, "({x},{y})");
            }
        }
        // A full-screen view (viewsize 120) passes through untouched.
        let full = calc_refdef(320, 200, 120.0, false);
        let view = Image::new(320, 200, [250, 1, 2]);
        let out = compose_view(view, full.vrect, 320, 200, Some(&tile), &pal);
        assert!(out.rgb.iter().all(|&p| p == [250, 1, 2]));
    }

    #[test]
    fn compose_view_on_a_dirty_spare_buffer_matches_a_fresh_full_clear() {
        // The screen is a recycled buffer left uncleared, so every pixel must
        // be written: compare with the straightforward compose (a fresh
        // screen, the tile everywhere, the view copied over it) with garbage
        // in every spare buffer first, for every viewsize, a few sizes, with
        // and without a backtile, and views hanging off the screen.
        let pal = ramp_palette();
        let tile = test_backtile();
        let reference = |view: &Image, vrect: ViewRect, w: usize, h: usize, t: Option<&Qpic>| {
            let mut img = Image::new(w, h, [0, 0, 0]);
            draw_tile_clear(&mut img, t, 0, 0, w, h, &pal);
            for vy in 0..view.h {
                for vx in 0..view.w {
                    img.put((vrect.x + vx) as i32, (vrect.y + vy) as i32, view.rgb[vy * view.w + vx]);
                }
            }
            img.rgb
        };
        let mut cases = Vec::new();
        for &(w, h) in &[(320, 200), (640, 400), (1280, 800), (400, 300)] {
            for vs in (30..=120).step_by(10) {
                cases.push((w, h, calc_refdef(w, h, vs as f32, false).vrect));
            }
        }
        cases.push((320, 200, ViewRect { x: 300, y: 190, w: 64, h: 32 }));
        cases.push((320, 200, ViewRect { x: 400, y: 10, w: 16, h: 16 }));
        cases.push((320, 200, ViewRect { x: 0, y: 0, w: 100, h: 300 }));
        for (i, &(w, h, vrect)) in cases.iter().enumerate() {
            let view = Image::new(vrect.w, vrect.h, [250, (i % 7) as u8, 2]);
            for t in [Some(&tile), None] {
                for _ in 0..3 {
                    crate::render::recycle_image(Image::new(w, h + 7, [1, 2, 3]));
                }
                let want = reference(&view, vrect, w, h, t);
                let got = compose_view(
                    Image { w: view.w, h: view.h, rgb: view.rgb.clone() },
                    vrect,
                    w,
                    h,
                    t,
                    &pal,
                );
                assert_eq!((got.w, got.h), (w, h));
                assert!(got.rgb == want, "{w}x{h} {vrect:?} tile {}", t.is_some());
            }
        }
    }
}
