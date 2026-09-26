//! The 2-D drawing primitives: pics, characters, strings, fade and tiles.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/draw.c` — `Draw_Pic`, `Draw_Character`, `Draw_String`,
//! `Draw_FadeScreen`, `Draw_TileClear`, and `Draw_Init`'s `conchars`.

use crate::render::Image;

/// The transparent palette index in Quake's HUD pics: texels equal to 255 are
/// skipped when blitting (`sbar.c` / `draw.c` treat 255 as see-through).
pub(crate) const HUD_TRANSPARENT: u8 = 255;

/// The virtual screen width Quake's `sbar.c` was authored against: the bar is
/// 320 wide, centred on wider screens.
pub(crate) const HUD_VIRT_W: f32 = 320.0;

thread_local! {
    /// The "scaled 2-D" extra ([`set_scaled_2d`]); off = id.
    static SCALED_2D: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Turn the "scaled 2-D" extra on or off. **Off is id** (the default): in
/// every video mode WinQuake draws the status bar, menus, console and text at
/// their own pixel size — a 320-wide bar centred at the bottom of a 640x480
/// screen with backtile either side, the menus at the top centre, a console
/// `vid.width/8 - 2` characters wide. **On** (an opt-in departure, for big
/// browser canvases) lays the 2-D layer out on a 320x200 screen, as id does
/// in mode 0, and blows every pixel of it up to fill the frame
/// ([`screen_2d`]). The 3-D view is unaffected either way, except that the
/// status bar it must clear grows with the scale.
pub fn set_scaled_2d(on: bool) {
    SCALED_2D.with(|c| c.set(on));
}

/// Whether the "scaled 2-D" extra is on ([`set_scaled_2d`]).
pub fn scaled_2d() -> bool {
    SCALED_2D.with(|c| c.get())
}

/// Tests: the "scaled 2-D" extra set for as long as the guard lives, then
/// back off (so a failing test cannot leak it into the next one on its thread).
#[cfg(test)]
pub(crate) struct Scaled2dGuard;

#[cfg(test)]
impl Scaled2dGuard {
    pub(crate) fn set(on: bool) -> Scaled2dGuard {
        set_scaled_2d(on);
        Scaled2dGuard
    }
}

#[cfg(test)]
impl Drop for Scaled2dGuard {
    fn drop(&mut self) {
        set_scaled_2d(false);
    }
}

/// The 2-D layer's geometry on a framebuffer: the screen id's 2-D code lays
/// out against (`vid.width` x `vid.height`) and the size of one of its pixels
/// in framebuffer pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen2d {
    /// Framebuffer pixels per 2-D pixel: 1 (id), or the extra's scale.
    pub scale: f32,
    /// `vid.width` and `vid.height` as the 2-D code sees them.
    pub w: i32,
    pub h: i32,
}

impl Screen2d {
    /// The framebuffer pixel of 2-D coordinate `v` (its top or left edge).
    pub fn px(&self, v: i32) -> i64 {
        (v as f32 * self.scale).floor() as i64
    }
}

/// The 2-D screen of a `vid_w x vid_h` framebuffer: the framebuffer itself at
/// scale 1 (id), or with the "scaled 2-D" extra ([`set_scaled_2d`]) the
/// largest scale at which a 320x200 screen fits, the layout then done on the
/// framebuffer divided by it — 320x200 on every 16:10 mode.
pub fn screen_2d(vid_w: usize, vid_h: usize) -> Screen2d {
    if scaled_2d() {
        let s = (vid_w as f32 / HUD_VIRT_W).min(vid_h as f32 / MENU_VIRT_H);
        if s.is_finite() && s > 1.0 {
            let w = ((vid_w as f32 / s).round() as i32).max(HUD_VIRT_W as i32);
            let h = ((vid_h as f32 / s).round() as i32).max(MENU_VIRT_H as i32);
            return Screen2d { scale: s, w, h };
        }
    }
    Screen2d { scale: 1.0, w: vid_w as i32, h: vid_h as i32 }
}

/// `Draw_TileClear` (draw.c): fill the framebuffer rectangle `(x, y, w, h)` with
/// the 64x64 `backtile` pic, tiled from the SCREEN origin (texel
/// `(x mod 64, y mod 64)`), as SCR_UpdateScreen does under a view smaller than
/// the screen. Like the rest of the 2-D layer the tile is drawn at the
/// [`screen_2d`] scale (1 unless the "scaled 2-D" extra is on). A missing or
/// malformed `backtile` fills black; every write is clipped.
pub fn draw_tile_clear(
    image: &mut Image,
    backtile: Option<&crate::wad::Qpic>,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
) {
    let x1 = x.saturating_add(w).min(image.w);
    let y1 = y.saturating_add(h).min(image.h);
    if x >= x1 || y >= y1 {
        return;
    }
    let tile = backtile.filter(|t| {
        t.width > 0 && t.height > 0 && t.data.len() >= (t.width as usize) * (t.height as usize)
    });
    let w = image.w;
    let Some(t) = tile else {
        for py in y..y1 {
            image.pixels[py * w + x..py * w + x1].fill(0);
        }
        return;
    };
    let scale = screen_2d(image.w, image.h).scale;
    let inv = if scale.is_finite() && scale > 0.0 { 1.0 / scale } else { 1.0 };
    let (tw, th) = (t.width as usize, t.height as usize);
    // The tile column of every framebuffer column, once per call.
    let cols: Vec<usize> = (x..x1).map(|px| ((px as f32 * inv) as usize) % tw).collect();
    // A row depends only on its tile row: one already drawn is copied.
    let mut prev: Option<(usize, usize)> = None; // (framebuffer row, tile row)
    for py in y..y1 {
        let ty = ((py as f32 * inv) as usize) % th;
        match prev {
            Some((prev_py, prev_ty)) if prev_ty == ty => {
                image.pixels.copy_within(prev_py * w + x..prev_py * w + x1, py * w + x);
            }
            _ => {
                let trow = &t.data[ty * tw..ty * tw + tw];
                for (out, &tx) in image.pixels[py * w + x..py * w + x1].iter_mut().zip(&cols) {
                    *out = trow[tx];
                }
            }
        }
        prev = Some((py, ty));
    }
}

/// Fill the framebuffer rectangle `[x0, x1) x [y0, y1)` (any coordinates;
/// clipped to the image) with `c`, a row slice at a time.
pub(crate) fn fill_rect(image: &mut Image, x0: i64, y0: i64, x1: i64, y1: i64, c: u8) {
    let (w, h) = (image.w as i64, image.h as i64);
    let (x0, x1) = (x0.clamp(0, w) as usize, x1.clamp(0, w) as usize);
    let (y0, y1) = (y0.clamp(0, h) as usize, y1.clamp(0, h) as usize);
    if x0 >= x1 {
        return;
    }
    for py in y0..y1 {
        if let Some(row) = image.pixels.get_mut(py * image.w + x0..py * image.w + x1) {
            row.fill(c);
        }
    }
}

/// The scaled, clipped, nearest-neighbour blit every 2-D pic and status-bar
/// glyph goes through (`Draw_Pic` / `Draw_TransPic` / `Draw_Character` at the
/// port's scale): the `sw x sh` rectangle at `(sx0, sy0)` of the 8-bit `src` (row
/// stride `stride`) becomes a `dst_w x dst_h` block with its top-left at
/// framebuffer `(dst_x0, dst_y0)`. Destination pixel `(dx, dy)` samples source
/// `((dx as f32 * inv_scale) as usize, (dy as f32 * inv_scale) as usize)`;
/// texels equal to `transparent` leave the pixel alone; everything off the
/// image or past the rectangle is skipped. The source-column map and the
/// clipping are worked out once per blit and each row is written as a slice
/// (PERF_PLAN B4: a float mapping and a bounds-checked `put` per pixel made
/// the status bar ~3x as expensive).
pub(crate) fn blit_scaled(
    image: &mut Image,
    src: &[u8],
    stride: usize,
    (sx0, sy0, sw, sh): (usize, usize, usize, usize),
    (dst_x0, dst_y0, dst_w, dst_h): (i64, i64, i64, i64),
    inv_scale: f32,
    transparent: u8,
) {
    let (iw, ih) = (image.w as i64, image.h as i64);
    // Destination columns on the image, then only while the source column is
    // inside the rectangle (the map never decreases, so that is a prefix).
    let dx_lo = (-dst_x0).clamp(0, dst_w.max(0));
    let dx_hi = (iw - dst_x0).clamp(dx_lo, dst_w.max(0));
    let cols: Vec<usize> = (dx_lo..dx_hi)
        .map(|dx| (dx as f32 * inv_scale) as usize)
        .take_while(|&sx| sx < sw)
        .collect();
    if cols.is_empty() {
        return;
    }
    let x_start = (dst_x0 + dx_lo) as usize;
    for dy in 0..dst_h {
        let py = dst_y0 + dy;
        if py < 0 || py >= ih {
            continue;
        }
        let sy = (dy as f32 * inv_scale) as usize;
        if sy >= sh {
            continue;
        }
        let row0 = (sy0 + sy) * stride + sx0;
        let Some(srow) = src.get(row0..row0 + sw) else { continue };
        let d0 = py as usize * image.w + x_start;
        let Some(drow) = image.pixels.get_mut(d0..d0 + cols.len()) else { continue };
        for (out, &sx) in drow.iter_mut().zip(&cols) {
            let t = srow[sx];
            if t != transparent {
                *out = t;
            }
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
pub(crate) fn blit_qpic_at(
    image: &mut Image,
    pic: &crate::wad::Qpic,
    vx: f32,
    vy: f32,
    scale: f32,
    ox: f32,
    oy: f32,
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
    blit_scaled(
        image,
        &pic.data,
        pw,
        (0, 0, pw, ph),
        (dst_x0, dst_y0, dst_w, dst_h),
        inv_scale,
        HUD_TRANSPARENT,
    );
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
) {
    draw_string_scaled(image, conchars, x as f32, y as f32, text, 1.0, 0.0, 0.0);
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
            stamp_glyph(image, conchars, ch, cell_w, cell_h, pen_vx, vy, scale, ox, oy);
        }
        pen_vx += 8.0; // M_Print advances the pen 8 virtual px per character.
    }
}

/// Stamp conchars glyph `num` (a `cell_w x cell_h` cell of the 16x16 grid)
/// with its top-left at virtual `(vx, vy)`, scaled by `scale` and offset by
/// `(ox, oy)`: every lit texel (palette index 0 is the glyph background)
/// becomes a `ceil(scale)`-square block at `floor(o + (v + g) * scale)`, so an
/// upscaled glyph is solid (at scale 1 a texel is one pixel). Blocks overlap
/// at a fractional scale; texels are stamped in row-major order, later over
/// earlier. The atlas being too small / the cell falling outside it is a
/// silent skip.
#[allow(clippy::too_many_arguments)]
fn stamp_glyph(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    num: u8,
    cell_w: usize,
    cell_h: usize,
    vx: f32,
    vy: f32,
    scale: f32,
    ox: f32,
    oy: f32,
) {
    let cw = conchars.width as usize;
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
            fill_rect(image, px, py, px + block, py + block, texel);
        }
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
    stamp_glyph(image, conchars, num, cell_w, cell_h, vx, vy, scale, ox, oy);
}

/// `Draw_FadeScreen` (draw.c), which `M_Draw` runs under every menu drawn over
/// the game or a demo: three pixels in four go to palette index 0 in a fixed
/// dither — row `y` keeps only the pixels with `x & 3 == (y & 1) << 1`. The
/// pattern is laid on the [`screen_2d`] pixels (the framebuffer's own unless
/// the "scaled 2-D" extra blows each up to a `scale x scale` block).
pub fn fade_screen(image: &mut Image) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let scale = screen_2d(image.w, image.h).scale;
    let inv = if scale.is_finite() && scale > 0.0 { 1.0 / scale } else { 1.0 };
    let black = 0;
    // Virtual column of each framebuffer column, computed once, and from it
    // the runs of columns a row keeps for each of the two patterns
    // (t = 0 on even virtual rows, 2 on odd): everything between them is
    // blackened a slice at a time.
    let vcols: Vec<usize> = (0..image.w).map(|x| (x as f32 * inv) as usize).collect();
    let keep_runs = |t: usize| {
        let mut runs: Vec<(usize, usize)> = Vec::new();
        for (x, &vx) in vcols.iter().enumerate() {
            if vx & 3 == t {
                match runs.last_mut() {
                    Some(r) if r.1 == x => r.1 = x + 1,
                    _ => runs.push((x, x + 1)),
                }
            }
        }
        runs
    };
    let runs = [keep_runs(0), keep_runs(2)];
    for y in 0..image.h {
        let vy = (y as f32 * inv) as usize;
        let row = &mut image.pixels[y * image.w..(y + 1) * image.w];
        let mut x = 0;
        for &(a, b) in &runs[vy & 1] {
            row[x..a].fill(black);
            x = b;
        }
        row[x..].fill(black);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::fixtures::test_backtile;

    #[test]
    fn fade_screen_blackens_three_pixels_in_four_like_draw_fadescreen() {
        // Draw_FadeScreen: row y keeps only x & 3 == (y & 1) << 1; the rest go
        // to palette index 0.
        let keep = 200u8;
        let mut img = Image::new(320, 200, keep);
        fade_screen(&mut img);
        for y in 0..200 {
            for x in 0..320 {
                let want = if x & 3 == (y & 1) << 1 { keep } else { 0 };
                assert_eq!(img.pixels[y * 320 + x], want, "({x},{y})");
            }
        }
        // id at 640x400: the same dither on the framebuffer's own pixels.
        let mut big = Image::new(640, 400, keep);
        fade_screen(&mut big);
        for &(x, y) in &[(0, 0), (1, 1), (4, 2), (6, 3), (2, 0), (0, 2), (639, 399)] {
            let want = if x & 3 == (y & 1) << 1 { keep } else { 0 };
            assert_eq!(big.pixels[y * 640 + x], want, "({x},{y})");
        }
        // The "scaled 2-D" extra at 640x400: each virtual pixel a 2x2 block.
        let _extra = Scaled2dGuard::set(true);
        let mut big = Image::new(640, 400, keep);
        fade_screen(&mut big);
        for &(x, y) in &[(0, 0), (1, 1), (4, 2), (5, 3), (2, 0), (0, 2), (639, 399)] {
            let (vx, vy) = (x / 2, y / 2);
            let want = if vx & 3 == (vy & 1) << 1 { keep } else { 0 };
            assert_eq!(big.pixels[y * 640 + x], want, "({x},{y})");
        }
        let kept = big.pixels.iter().filter(|&&p| p == keep).count();
        assert_eq!(kept, 640 * 400 / 4, "a quarter of the screen survives");
        fade_screen(&mut Image::new(0, 0, keep));
    }

    #[test]
    fn draw_string_writes_glyph_pixels() {
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

        let mut img = Image::new(64, 16, 0);
        draw_string(&mut img, &conchars, 0, 0, "A");
        // 'A' = byte 65 = cell (1, 4): source (8, 32). Its texels are lit (index 3),
        // stamped at the destination starting (0,0). So pixel (0,0) is index 3.
        assert_eq!(img.pixels[0], 3, "the glyph's lit texel must paint");
        // A space draws nothing past the first char; draw a string and confirm the
        // second char ('B') lands 8 px to the right.
        let mut img2 = Image::new(64, 16, 0);
        draw_string(&mut img2, &conchars, 0, 0, " B");
        // Space is skipped, 'B' starts at virtual x=8.
        assert_eq!(img2.pixels[8], 3, "the second glyph must land 8px right");
        assert_eq!(img2.pixels[0], 0, "a leading space must draw nothing");
    }

    // -- the row-wise blits against the per-pixel loops they replaced --------

    /// A deterministic pseudo-random byte stream.
    fn bytes(seed: u32, n: usize) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x >> 3) as u8
            })
            .collect()
    }

    /// The old `blit_qpic_at` / `sbar::blit_qpic` body: per pixel, float
    /// source mapping and a bounds-checked `put`.
    fn ref_blit(img: &mut Image, pic: &crate::wad::Qpic, x0: i64, y0: i64, scale: f32, t: u8) {
        let (pw, ph) = (pic.width as usize, pic.height as usize);
        let dst_w = (pw as f32 * scale).round().max(1.0) as i64;
        let dst_h = (ph as f32 * scale).round().max(1.0) as i64;
        let inv = 1.0 / scale;
        for dy in 0..dst_h {
            let py = y0 + dy;
            let sy = (dy as f32 * inv) as usize;
            if py < 0 || py >= img.h as i64 || sy >= ph {
                continue;
            }
            for dx in 0..dst_w {
                let px = x0 + dx;
                let sx = (dx as f32 * inv) as usize;
                if px < 0 || px >= img.w as i64 || sx >= pw {
                    continue;
                }
                let texel = pic.data[sy * pw + sx];
                if texel != t {
                    img.put(px as i32, py as i32, texel);
                }
            }
        }
    }

    const SCALES: [f32; 10] = [0.3, 0.5, 1.0, 1.25, 1.37, 2.0, 2.5, 3.2, 4.0, 5.333];

    #[test]
    fn blit_qpic_at_matches_the_per_pixel_blit() {
        let mut seed = 1;
        for &(pw, ph) in &[(1, 1), (7, 5), (24, 24), (33, 17), (320, 24)] {
            // Every fourth texel transparent.
            let data: Vec<u8> = bytes(seed, pw * ph)
                .into_iter()
                .map(|b| if b % 4 == 0 { HUD_TRANSPARENT } else { b })
                .collect();
            let pic = crate::wad::Qpic { width: pw as i32, height: ph as i32, data };
            for &scale in &SCALES {
                for &(vx, vy, ox, oy) in &[
                    (0.0, 0.0, 0.0, 0.0),
                    (10.5, 3.25, 7.5, 1.5),
                    (-9.0, -4.0, 0.0, 0.0),
                    (60.0, 40.0, -3.0, 2.0),
                    (0.0, 0.0, 91.0, 57.0),
                ] {
                    seed += 1;
                    let mut want = Image { w: 97, h: 61, pixels: bytes(seed, 97 * 61) };
                    let mut got = want.clone();
                    let x0 = (ox + vx * scale).floor() as i64;
                    let y0 = (oy + vy * scale).floor() as i64;
                    ref_blit(&mut want, &pic, x0, y0, scale, HUD_TRANSPARENT);
                    blit_qpic_at(&mut got, &pic, vx, vy, scale, ox, oy);
                    assert!(got.pixels == want.pixels, "{pw}x{ph} scale {scale} at ({vx},{vy})+({ox},{oy})");
                }
            }
        }
    }

    #[test]
    fn glyph_stamping_matches_the_per_pixel_blocks() {
        // The old draw_string_scaled/draw_char_scaled inner loop: a
        // ceil(scale)-square of puts per lit texel, texels in row-major order.
        let conchars =
            crate::wad::Qpic { width: 128, height: 128, data: bytes(7, 128 * 128).iter().map(|b| b % 3).collect() };
        for &scale in &SCALES {
            for &(vx, vy, ox, oy) in &[(0.0, 0.0, 0.0, 0.0), (3.5, 2.25, 1.5, 0.5), (-12.0, -3.0, 0.0, 0.0), (70.0, 30.0, 2.0, 1.0)] {
                let text = "Az 09~\x7f\u{1}";
                let mut want = Image::new(120, 50, 9);
                let mut got = Image::new(120, 50, 9);
                let block = scale.ceil().max(1.0) as i64;
                let mut pen = vx;
                for ch in text.bytes() {
                    if ch != 0 && ch != b' ' {
                        let (cx, cy) = ((ch as usize % 16) * 8, (ch as usize / 16) * 8);
                        for gy in 0..8 {
                            let py = (oy + (vy + gy as f32) * scale).floor() as i64;
                            for gx in 0..8 {
                                let texel = conchars.data[(cy + gy) * 128 + cx + gx];
                                if texel == 0 {
                                    continue;
                                }
                                let px = (ox + (pen + gx as f32) * scale).floor() as i64;
                                for by in 0..block {
                                    for bx in 0..block {
                                        want.put((px + bx) as i32, (py + by) as i32, texel);
                                    }
                                }
                            }
                        }
                    }
                    pen += 8.0;
                }
                draw_string_scaled(&mut got, &conchars, vx, vy, text, scale, ox, oy);
                assert!(got.pixels == want.pixels, "string at scale {scale} ({vx},{vy})+({ox},{oy})");
                // draw_char_scaled stamps the same cells, byte 0 and space included.
                let mut one = Image::new(120, 50, 9);
                let mut two = Image::new(120, 50, 9);
                draw_char_scaled(&mut one, &conchars, vx, vy, b'Q', scale, ox, oy);
                draw_string_scaled(&mut two, &conchars, vx, vy, "Q", scale, ox, oy);
                assert!(one.pixels == two.pixels, "char at scale {scale}");
            }
        }
    }

    #[test]
    fn fade_screen_matches_the_per_pixel_dither_at_any_scale() {
        for (scaled, &(w, h)) in [false, true]
            .into_iter()
            .flat_map(|e| [(320, 200), (333, 211), (400, 300), (960, 600), (1120, 700), (1280, 800), (7, 3)].iter().map(move |r| (e, r)))
        {
            let _extra = Scaled2dGuard::set(scaled);
            let under = bytes(w as u32, w * h);
            let scale = if scaled { (w as f32 / MENU_VIRT_W).min(h as f32 / MENU_VIRT_H).max(1.0) } else { 1.0 };
            let inv = 1.0 / scale;
            let mut want = under.clone();
            for y in 0..h {
                let t = (((y as f32 * inv) as usize) & 1) << 1;
                for x in 0..w {
                    if ((x as f32 * inv) as usize) & 3 != t {
                        want[y * w + x] = 0;
                    }
                }
            }
            let mut got = Image { w, h, pixels: under };
            fade_screen(&mut got);
            assert!(got.pixels == want, "{w}x{h} scaled {scaled}");
        }
    }

    #[test]
    fn draw_tile_clear_matches_the_per_pixel_tile() {
        let tile = test_backtile();
        let odd = crate::wad::Qpic { width: 13, height: 7, data: bytes(3, 13 * 7) };
        for (scaled, t) in [(false, &tile), (false, &odd), (true, &tile), (true, &odd)] {
            let _extra = Scaled2dGuard::set(scaled);
            for &(w, h) in &[(320, 200), (400, 300), (640, 400), (1120, 700), (1280, 800), (333, 211)] {
                for &(x, y, rw, rh) in &[(0, 0, w, h), (5, 7, w / 3, h / 2), (w - 9, h - 4, 40, 40), (0, h / 2, w, 1)] {
                    let mut want = Image::new(w, h, 7);
                    let inv = if scaled { 1.0 / (w as f32 / HUD_VIRT_W) } else { 1.0 };
                    let (tw, th) = (t.width as usize, t.height as usize);
                    for py in y..(y + rh).min(h) {
                        for px in x..(x + rw).min(w) {
                            let tx = ((px as f32 * inv) as usize) % tw;
                            let ty = ((py as f32 * inv) as usize) % th;
                            want.pixels[py * w + px] = t.data[ty * tw + tx];
                        }
                    }
                    let mut got = Image::new(w, h, 7);
                    draw_tile_clear(&mut got, Some(t), x, y, rw, rh);
                    assert!(got.pixels == want.pixels, "{w}x{h} rect ({x},{y},{rw},{rh}) tile {}x{}", t.width, t.height);
                }
            }
        }
    }

    #[test]
    fn draw_tile_clear_tiles_from_the_screen_origin() {
        let tile
 = test_backtile();
        let at = |x: usize, y: usize| tile.data[(y % 64) * 64 + x % 64];
        // Scale 1 (320 wide): texel (x mod 64, y mod 64) — anchored at the
        // SCREEN origin, not the rectangle's corner (Draw_TileClear's offsets).
        let mut img = Image::new(320, 200, 7);
        draw_tile_clear(&mut img, Some(&tile), 70, 30, 100, 50);
        assert_eq!(img.pixels[30 * 320 + 70], at(70, 30));
        assert_eq!(img.pixels[79 * 320 + 169], at(169, 79));
        assert_eq!(img.pixels[29 * 320 + 70], 7, "outside the rect untouched");
        assert_eq!(img.pixels[30 * 320 + 170], 7, "outside the rect untouched");
        // 640 wide: id tiles every mode at the tile's own size ...
        let mut big = Image::new(640, 400, 7);
        draw_tile_clear(&mut big, Some(&tile), 0, 0, 640, 400);
        for &(x, y) in &[(0, 0), (1, 1), (129, 3), (300, 250), (639, 399)] {
            assert_eq!(big.pixels[y * 640 + x], at(x, y), "({x},{y})");
        }
        // ... the "scaled 2-D" extra makes each texel 2x2 pixels, like the
        // rest of its 2-D layer.
        let _extra = Scaled2dGuard::set(true);
        draw_tile_clear(&mut big, Some(&tile), 0, 0, 640, 400);
        for &(x, y) in &[(0, 0), (1, 1), (129, 3), (300, 250), (639, 399)] {
            assert_eq!(big.pixels[y * 640 + x], at(x / 2, y / 2), "({x},{y})");
        }
        // No tile: black, never a panic; an off-frame rect is a no-op.
        let mut img2 = Image::new(32, 32, 7);
        draw_tile_clear(&mut img2, None, 0, 0, 32, 32);
        assert!(img2.pixels.iter().all(|&p| p == 0));
        draw_tile_clear(&mut img2, Some(&tile), 40, 40, 10, 10);
    }
}
