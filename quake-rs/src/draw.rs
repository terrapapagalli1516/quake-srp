//! The 2-D drawing primitives: pics, characters, strings, fade and tiles.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/draw.c` — `Draw_Pic`, `Draw_Character`, `Draw_String`,
//! `Draw_FadeScreen`, `Draw_TileClear`, and `Draw_Init`'s `conchars`.

use crate::render::Image;

/// The transparent palette index in Quake's HUD pics: texels equal to 255 are
/// skipped when blitting (`sbar.c` / `draw.c` treat 255 as see-through).
pub(crate) const HUD_TRANSPARENT: u8 = 255;

/// The virtual screen width Quake's `sbar.c` was authored against. The whole bar
/// is laid out in this 320-wide space, then scaled to the real framebuffer.
pub(crate) const HUD_VIRT_W: f32 = 320.0;

/// `Draw_TileClear` (draw.c): fill the framebuffer rectangle `(x, y, w, h)` with
/// the 64x64 `backtile` pic, tiled from the SCREEN origin (texel
/// `(x mod 64, y mod 64)`), as SCR_UpdateScreen does under a view smaller than
/// the screen. Like the rest of the 2-D layer the tile is the 320x200 virtual
/// screen's, scaled by `vid_w/320` (nearest-neighbour). A missing or malformed
/// `backtile` fills black; every write is clipped.
pub fn draw_tile_clear(
    image: &mut Image,
    backtile: Option<&crate::wad::Qpic>,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    palette: &[[u8; 3]; 256],
) {
    let x1 = x.saturating_add(w).min(image.w);
    let y1 = y.saturating_add(h).min(image.h);
    if x >= x1 || y >= y1 {
        return;
    }
    let tile = backtile.filter(|t| {
        t.width > 0 && t.height > 0 && t.data.len() >= (t.width as usize) * (t.height as usize)
    });
    let scale = image.w as f32 / HUD_VIRT_W;
    let inv = if scale.is_finite() && scale > 0.0 { 1.0 / scale } else { 1.0 };
    for py in y..y1 {
        let row = &mut image.rgb[py * image.w..py * image.w + image.w];
        let Some(t) = tile else {
            row[x..x1].fill([0, 0, 0]);
            continue;
        };
        let (tw, th) = (t.width as usize, t.height as usize);
        let ty = ((py as f32 * inv) as usize) % th;
        let trow = &t.data[ty * tw..ty * tw + tw];
        for (px, out) in row.iter_mut().enumerate().take(x1).skip(x) {
            let tx = ((px as f32 * inv) as usize) % tw;
            *out = palette[trow[tx] as usize];
        }
    }
}

/// Fetch the raw 128x128 `conchars` console font from `wad` as a [`crate::wad::Qpic`].
///
/// Unlike the HUD's `num_*`/`sbar` pics, `conchars` is a *headerless* lump (a flat
/// 128x128 byte block, no QPIC width/height prefix), so it is read via
/// `lump`/`lump_data` and wrapped with `width = height = 128` — exactly how
/// `quaketool`'s menu path builds it. A missing/short lump yields `None` and the
/// ammo-count text simply doesn't draw (graceful degrade).
pub fn conchars_pic(wad: &crate::wad::Wad2) -> Option<crate::wad::Qpic> {
    let lump = wad.lump("conchars")?;
    let data = wad.lump_data(lump).ok()?;
    if data.len() < 128 * 128 {
        return None;
    }
    Some(crate::wad::Qpic {
        width: 128,
        height: 128,
        data: data[..128 * 128].to_vec(),
    })
}

/// The virtual screen width/height the menu is authored against (Quake's fixed
/// 320x200 layout). `M_DrawPic`/`M_DrawTransPic` center this in the real screen
/// via `(vid.width - 320) >> 1`; here [`draw_menu`](crate::menu::draw_menu) scales/centers instead.
pub(crate) const MENU_VIRT_W: f32 = 320.0;
pub(crate) const MENU_VIRT_H: f32 = 200.0;

/// Blit one `Qpic` with its top-left at virtual `(vx, vy)` in [`MENU_VIRT_W`] x
/// [`MENU_VIRT_H`] space, scaled by `scale` and offset by `(ox, oy)` framebuffer
/// pixels (so the virtual canvas can be centered in a wider/taller frame).
/// Index-255 texels are transparent; every write clips at the framebuffer edge.
///
/// This is the top-left-anchored sibling of `sbar::blit_qpic` (which bottom-anchors
/// the HUD). At `scale = 1.0`, `ox = oy = 0` a virtual `(vx, vy)` lands at the
/// framebuffer pixel `(vx, vy)` — the case the 320x200 wasm framebuffer uses, so
/// the menu coordinates from menu.c are used directly with no transform.
// Mirrors Draw_Pic (draw.c); the C reads vid/draw globals passed explicitly here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn blit_qpic_at(
    image: &mut Image,
    pic: &crate::wad::Qpic,
    vx: f32,
    vy: f32,
    scale: f32,
    ox: f32,
    oy: f32,
    palette: &[[u8; 3]; 256],
) {
    if pic.width <= 0 || pic.height <= 0 || scale <= 0.0 {
        return;
    }
    let pw = pic.width as usize;
    let ph = pic.height as usize;
    if pic.data.len() < pw.saturating_mul(ph) {
        return;
    }

    let dst_x0 = (ox + vx * scale).floor() as i64;
    let dst_y0 = (oy + vy * scale).floor() as i64;
    let dst_w = (pw as f32 * scale).round().max(1.0) as i64;
    let dst_h = (ph as f32 * scale).round().max(1.0) as i64;
    let inv_scale = 1.0 / scale;

    for dy in 0..dst_h {
        let py = dst_y0 + dy;
        if py < 0 || py >= image.h as i64 {
            continue;
        }
        let sy = (dy as f32 * inv_scale) as usize;
        if sy >= ph {
            continue;
        }
        for dx in 0..dst_w {
            let px = dst_x0 + dx;
            if px < 0 || px >= image.w as i64 {
                continue;
            }
            let sx = (dx as f32 * inv_scale) as usize;
            if sx >= pw {
                continue;
            }
            let texel = match pic.data.get(sy * pw + sx) {
                Some(&t) => t,
                None => continue,
            };
            if texel == HUD_TRANSPARENT {
                continue;
            }
            image.put(px as i32, py as i32, palette[texel as usize]);
        }
    }
}

/// Draw a string of console characters using the 128x128 `conchars` font atlas, a
/// port of Quake's `Draw_String`/`Draw_Character` (`draw.c`).
///
/// `conchars` is the 16x16 grid of 8x8 glyphs (so byte `c`'s glyph sits at cell
/// `(c % 16, c / 16)`, i.e. source pixel `(8*(c%16), 8*(c/16))`). Each character of
/// `text` is stamped 8 virtual pixels apart starting at virtual `(x, y)` in
/// 320x200 space, scaled by `scale` and offset by `(ox, oy)` framebuffer pixels —
/// the same transform [`blit_qpic_at`] uses, so font text lines up with the menu
/// pics.
///
/// Glyph index 0 (the transparent "space" cell whose texels are palette index 0)
/// and the ASCII space are skipped without drawing. Glyph texels equal to palette
/// index 0 are treated as transparent (the conchars atlas uses 0 for the glyph
/// background). The atlas being too small / a glyph cell falling outside it is a
/// silent skip — never a panic.
///
/// The conchars lump in `gfx.wad` is a raw 128x128 byte block (no QPIC header);
/// callers wrap it as a [`crate::wad::Qpic`] with `width = height = 128` and the
/// 16384 lump bytes as `data`.
pub fn draw_string(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    x: i32,
    y: i32,
    text: &str,
    palette: &[[u8; 3]; 256],
) {
    draw_string_scaled(image, conchars, x as f32, y as f32, text, 1.0, 0.0, 0.0, palette);
}

/// The scaled/offset core of [`draw_string`]; the menu draw uses this to place
/// labels in the same scaled+centered virtual space as the pics.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_string_scaled(
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
    if conchars.width <= 0 || conchars.height <= 0 || scale <= 0.0 {
        return;
    }
    let cw = conchars.width as usize;
    if conchars.data.len() < cw.saturating_mul(conchars.height as usize) {
        return;
    }
    // The atlas is a 16x16 grid; each glyph is (width/16)x(height/16) source px.
    let cell_w = (conchars.width / 16).max(1) as usize;
    let cell_h = (conchars.height / 16).max(1) as usize;

    let mut pen_vx = vx;
    for ch in text.bytes() {
        // Skip the transparent "space" glyphs (byte 0 and ASCII space): they only
        // hold palette-0 texels, so drawing them is a no-op anyway — but skipping
        // is cheaper and matches the menu's M_Print spacing.
        if ch != 0 && ch != b' ' {
            let cell_x = (ch as usize % 16) * cell_w;
            let cell_y = (ch as usize / 16) * cell_h;
            for gy in 0..cell_h {
                let sy = cell_y + gy;
                if sy >= conchars.height as usize {
                    break;
                }
                let py = (oy + (vy + gy as f32) * scale).floor() as i64;
                for gx in 0..cell_w {
                    let sx = cell_x + gx;
                    if sx >= cw {
                        break;
                    }
                    let texel = match conchars.data.get(sy * cw + sx) {
                        Some(&t) => t,
                        None => continue,
                    };
                    // The conchars atlas uses palette index 0 as the glyph's
                    // transparent background; only stamp the lit texels.
                    if texel == 0 {
                        continue;
                    }
                    let px = (ox + (pen_vx + gx as f32) * scale).floor() as i64;
                    // Stamp a scale x scale block so the glyph is solid when
                    // upscaled (nearest-neighbour); at scale 1 this is one pixel.
                    let block = scale.ceil().max(1.0) as i64;
                    for by in 0..block {
                        for bx in 0..block {
                            image.put((px + bx) as i32, (py + by) as i32, palette[texel as usize]);
                        }
                    }
                }
            }
        }
        pen_vx += 8.0; // M_Print advances the pen 8 virtual px per character.
    }
}

/// Draw ONE conchars glyph by its raw byte index (`M_DrawCharacter`), at virtual
/// `(vx, vy)` in 320x200 space, scaled+offset like [`draw_string_scaled`]. Unlike
/// `draw_string_scaled` (which speaks ASCII and skips byte 0 / space) this stamps
/// the exact cell `num`, so it can draw the slider strip (glyphs 128/129/130/131)
/// and the flashing cursor (glyphs 12/13). Palette-0 texels stay transparent; the
/// glyph cell falling outside the atlas is a silent skip.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_char_scaled(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    vx: f32,
    vy: f32,
    num: u8,
    scale: f32,
    ox: f32,
    oy: f32,
    palette: &[[u8; 3]; 256],
) {
    if conchars.width <= 0 || conchars.height <= 0 || scale <= 0.0 {
        return;
    }
    let cw = conchars.width as usize;
    if conchars.data.len() < cw.saturating_mul(conchars.height as usize) {
        return;
    }
    let cell_w = (conchars.width / 16).max(1) as usize;
    let cell_h = (conchars.height / 16).max(1) as usize;
    let cell_x = (num as usize % 16) * cell_w;
    let cell_y = (num as usize / 16) * cell_h;
    let block = scale.ceil().max(1.0) as i64;
    for gy in 0..cell_h {
        let sy = cell_y + gy;
        if sy >= conchars.height as usize {
            break;
        }
        let py = (oy + (vy + gy as f32) * scale).floor() as i64;
        for gx in 0..cell_w {
            let sx = cell_x + gx;
            if sx >= cw {
                break;
            }
            let texel = match conchars.data.get(sy * cw + sx) {
                Some(&t) => t,
                None => continue,
            };
            if texel == 0 {
                continue;
            }
            let px = (ox + (vx + gx as f32) * scale).floor() as i64;
            for by in 0..block {
                for bx in 0..block {
                    image.put((px + bx) as i32, (py + by) as i32, palette[texel as usize]);
                }
            }
        }
    }
}

/// `Draw_FadeScreen` (draw.c), which `M_Draw` runs under every menu drawn over
/// the game or a demo: three pixels in four go to palette index 0 in a fixed
/// dither — row `y` keeps only the pixels with `x & 3 == (y & 1) << 1`. The
/// pattern is laid on the 320x200 virtual screen, scaled like the rest of the
/// 2-D layer (each virtual pixel a `scale x scale` block).
pub fn fade_screen(image: &mut Image, palette: &[[u8; 3]; 256]) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let scale = (image.w as f32 / MENU_VIRT_W).min(image.h as f32 / MENU_VIRT_H);
    let inv = if scale.is_finite() && scale > 0.0 { 1.0 / scale } else { 1.0 };
    let black = palette[0];
    // Virtual column of each framebuffer column, computed once.
    let vcols: Vec<usize> = (0..image.w).map(|x| (x as f32 * inv) as usize).collect();
    for y in 0..image.h {
        let vy = (y as f32 * inv) as usize;
        let t = (vy & 1) << 1;
        let row = &mut image.rgb[y * image.w..(y + 1) * image.w];
        for (px, &vx) in row.iter_mut().zip(vcols.iter()) {
            if vx & 3 != t {
                *px = black;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::fixtures::{ramp_palette, test_backtile};

    #[test]
    fn fade_screen_blackens_three_pixels_in_four_like_draw_fadescreen() {
        // Draw_FadeScreen: row y keeps only x & 3 == (y & 1) << 1; the rest go
        // to palette index 0.
        let mut pal = ramp_palette();
        pal[0] = [1, 2, 3];
        let keep = [200u8, 100, 50];
        let mut img = Image::new(320, 200, keep);
        fade_screen(&mut img, &pal);
        for y in 0..200 {
            for x in 0..320 {
                let want = if x & 3 == (y & 1) << 1 { keep } else { pal[0] };
                assert_eq!(img.rgb[y * 320 + x], want, "({x},{y})");
            }
        }
        // Scaled 2-D layer: at 640x400 each virtual pixel is a 2x2 block.
        let mut big = Image::new(640, 400, keep);
        fade_screen(&mut big, &pal);
        for &(x, y) in &[(0, 0), (1, 1), (4, 2), (5, 3), (2, 0), (0, 2), (639, 399)] {
            let (vx, vy) = (x / 2, y / 2);
            let want = if vx & 3 == (vy & 1) << 1 { keep } else { pal[0] };
            assert_eq!(big.rgb[y * 640 + x], want, "({x},{y})");
        }
        let kept = big.rgb.iter().filter(|&&p| p == keep).count();
        assert_eq!(kept, 640 * 400 / 4, "a quarter of the screen survives");
        fade_screen(&mut Image::new(0, 0, keep), &pal);
    }

    #[test]
    fn draw_string_writes_glyph_pixels() {
        let pal = ramp_palette();
        // A 128x128 conchars where every texel is the lit index 3 EXCEPT the
        // space cell (byte 32 -> cell (0,2)) which stays at the transparent 0.
        // With an all-lit atlas, any non-space character stamps index-3 pixels.
        let mut data = vec![3u8; 128 * 128];
        // Zero out the byte-0 cell (top-left 8x8) so it's transparent.
        for y in 0..8 {
            for x in 0..8 {
                data[y * 128 + x] = 0;
            }
        }
        let conchars = crate::wad::Qpic { width: 128, height: 128, data };

        let mut img = Image::new(64, 16, [0, 0, 0]);
        draw_string(&mut img, &conchars, 0, 0, "A", &pal);
        // 'A' = byte 65 = cell (1, 4): source (8, 32). Its texels are lit (index 3),
        // stamped at the destination starting (0,0). So pixel (0,0) is index 3.
        assert_eq!(img.rgb[0], pal[3], "the glyph's lit texel must paint");
        // A space draws nothing past the first char; draw a string and confirm the
        // second char ('B') lands 8 px to the right.
        let mut img2 = Image::new(64, 16, [0, 0, 0]);
        draw_string(&mut img2, &conchars, 0, 0, " B", &pal);
        // Space is skipped, 'B' starts at virtual x=8.
        assert_eq!(img2.rgb[8], pal[3], "the second glyph must land 8px right");
        assert_eq!(img2.rgb[0], [0, 0, 0], "a leading space must draw nothing");
    }

    #[test]
    fn draw_tile_clear_tiles_from_the_screen_origin() {
        let pal = ramp_palette();
        let tile = test_backtile();
        let at = |x: usize, y: usize| pal[tile.data[(y % 64) * 64 + x % 64] as usize];
        // Scale 1 (320 wide): texel (x mod 64, y mod 64) — anchored at the
        // SCREEN origin, not the rectangle's corner (Draw_TileClear's offsets).
        let mut img = Image::new(320, 200, [7, 7, 7]);
        draw_tile_clear(&mut img, Some(&tile), 70, 30, 100, 50, &pal);
        assert_eq!(img.rgb[30 * 320 + 70], at(70, 30));
        assert_eq!(img.rgb[79 * 320 + 169], at(169, 79));
        assert_eq!(img.rgb[29 * 320 + 70], [7, 7, 7], "outside the rect untouched");
        assert_eq!(img.rgb[30 * 320 + 170], [7, 7, 7], "outside the rect untouched");
        // Scale 2 (640 wide): each texel covers 2x2 pixels, like the rest of
        // the scaled 2-D layer.
        let mut big = Image::new(640, 400, [7, 7, 7]);
        draw_tile_clear(&mut big, Some(&tile), 0, 0, 640, 400, &pal);
        for &(x, y) in &[(0, 0), (1, 1), (129, 3), (300, 250), (639, 399)] {
            assert_eq!(big.rgb[y * 640 + x], at(x / 2, y / 2), "({x},{y})");
        }
        // No tile: black, never a panic; an off-frame rect is a no-op.
        let mut img2 = Image::new(32, 32, [7, 7, 7]);
        draw_tile_clear(&mut img2, None, 0, 0, 32, 32, &pal);
        assert!(img2.rgb.iter().all(|&p| p == [0, 0, 0]));
        draw_tile_clear(&mut img2, Some(&tile), 40, 40, 10, 10, &pal);
    }
}
