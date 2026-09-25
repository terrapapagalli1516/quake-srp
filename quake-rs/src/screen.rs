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

/// What `SCR_CalcRefdef` works out each time the view changes: where the 3-D
/// view goes (`r_refdef.vrect`) and how many status-bar lines are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refdef {
    /// `r_refdef.vrect`: the 3-D view rectangle in framebuffer pixels. The
    /// renderer draws into a `vrect.w x vrect.h` image with the projection
    /// centred on it and `fov_x` spanning its width — R_ViewChanged's
    /// `xcenter = vrect.width/2 + vrect.x`, `xscale = vrect.width / (2 tan(fov_x/2))`,
    /// `yscale = xscale` (square pixels); the software renderer derives its
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
    // SCR_CalcRefdef: bound viewsize (a non-number reads as the default).
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
    let vw = vid_w as i64;
    let vh = vid_h as i64;
    let scale = vid_w as f32 / HUD_VIRT_W;
    let mut lineadj = ((sb_lines as f32 * scale).ceil() as i64).clamp(0, vh);

    // R_SetVrect (r_main.c).
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
    Refdef {
        vrect: ViewRect { x: x as usize, y: y as usize, w: w as usize, h: height as usize },
        sb_lines,
    }
}

/// Put the rendered 3-D `view` (a `vrect.w x vrect.h` image) into a
/// `vid_w x vid_h` screen at `vrect`, with everything outside it tile-cleared
/// ([`draw_tile_clear`] — SCR_UpdateScreen's `Draw_TileClear(0,0,vid.width,
/// vid.height)` under the view). A view that already IS the whole screen
/// (viewsize 120, an intermission) comes back untouched, at zero cost. The
/// status bar is drawn over the result afterwards, as in the C.
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
    let mut img = Image::new(vid_w, vid_h, [0, 0, 0]);
    draw_tile_clear(&mut img, backtile, 0, 0, vid_w, vid_h, palette);
    let cw = view.w.min(vid_w.saturating_sub(vrect.x));
    for vy in 0..view.h {
        let py = vrect.y + vy;
        if py >= vid_h {
            break;
        }
        let dst = py * vid_w + vrect.x;
        img.rgb[dst..dst + cw].copy_from_slice(&view.rgb[vy * view.w..vy * view.w + cw]);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::RESOLUTION_PRESETS;
    use crate::render::fixtures::{ramp_palette, solid_conchars, test_backtile};
    use crate::sbar::draw_finale_overlay;
    use crate::wad::Qpic;

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
}
