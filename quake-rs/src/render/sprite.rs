//! Sprite (SPR) entities.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/r_sprite.c` (`R_DrawSprite`, `R_GetSpriteframe`) and
//! `WinQuake/d_sprite.c`.

use crate::math::{dot, sub, Vec3};
use super::{Camera, Image};

/// A sprite-model entity to draw as a camera-facing billboard (Quake's
/// `mod_sprite` entities: the `s_explod.spr` explosion flash, bubbles, etc.).
pub struct SpriteInstance<'a> {
    pub sprite: &'a crate::spr::Sprite,
    /// World position of the sprite centre (the entity origin).
    pub origin: Vec3,
    /// Top-level frame index (clamped); a group frame animates by `time`.
    pub frame: usize,
}

/// Draw camera-facing sprite-model entities (Quake `mod_sprite`), z-tested against
/// the shared `zbuf`. Each sprite's active frame (a [`crate::spr::Frame::Single`]
/// or a `Group` whose sub-frame is selected by `time`) is rasterised as a
/// screen-aligned billboard: the sprite faces the camera, so a frame W×H pixels (1
/// texel = 1 world unit) spans `focal*W/vz` × `focal*H/vz` framebuffer pixels around
/// the projected origin, offset by the frame's `origin` (left/up). Palette index 255
/// is transparent (`d_sprite.c`). Nearest-neighbour sampled; behind-wall pixels are
/// hidden by the depth test (`vz < zbuf`) and write depth so nearer geometry wins.
/// Oriented sprites fall back to the facing billboard (good enough for shareware).
// Mirrors R_DrawSprite (r_sprite.c); the C reads globals (vid, r_refdef, cl.time)
// that this port passes explicitly.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_sprites(
    image: &mut Image,
    zbuf: &mut [f32],
    cam: &Camera,
    sprites: &[SpriteInstance],
    palette: &[[u8; 3]; 256],
    time: f32,
    w: usize,
    h: usize,
) {
    const NEAR: f32 = 1.0;
    if w == 0 || h == 0 || sprites.is_empty() {
        return;
    }
    let (forward, right, up) = cam.basis();
    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;
    let half_fov = (cam.fov_deg as f64 * 0.5).to_radians();
    let tan_half = half_fov.tan();
    let focal = if tan_half.abs() < 1e-6 {
        cx
    } else {
        (cx as f64 / tan_half) as f32
    };

    for inst in sprites {
        let Some(frame) = select_sprite_frame(inst.sprite, inst.frame, time) else {
            continue;
        };
        if frame.width <= 0 || frame.height <= 0 {
            continue;
        }
        let rel = sub(inst.origin, cam.pos);
        let vz = dot(rel, forward);
        if vz <= NEAR {
            continue; // at/behind the near plane
        }
        let sx = cx + focal * dot(rel, right) / vz;
        let sy = cy - focal * dot(rel, up) / vz;
        // 1 texel = 1 world unit; the facing billboard scales by focal/vz. The frame
        // `origin` is the left/up offset of its top-left from the centre (Quake:
        // up = origin[1], down = origin[1]-height, left = origin[0]).
        let scale = focal / vz;
        let (fw, fh) = (frame.width as f32, frame.height as f32);
        let (ox, oy) = (frame.origin[0] as f32, frame.origin[1] as f32);
        // +up is -screen-y; the top edge is at world up-offset `oy`.
        let x0 = sx + ox * scale;
        let x1 = sx + (ox + fw) * scale;
        let y0 = sy - oy * scale;
        let y1 = sy - (oy - fh) * scale;
        let (px0, px1) = (x0.min(x1), x0.max(x1));
        let (py0, py1) = (y0.min(y1), y0.max(y1));
        if !(px0.is_finite() && px1.is_finite() && py0.is_finite() && py1.is_finite()) {
            continue;
        }
        let ix0 = px0.floor().max(0.0) as usize;
        let ix1 = (px1.ceil() as i64).clamp(0, w as i64) as usize;
        let iy0 = py0.floor().max(0.0) as usize;
        let iy1 = (py1.ceil() as i64).clamp(0, h as i64) as usize;
        let span_x = (px1 - px0).max(1e-6);
        let span_y = (py1 - py0).max(1e-6);
        let (tw, th) = (frame.width as usize, frame.height as usize);
        for py in iy0..iy1 {
            // Texel row: fraction down the screen rect -> 0..th-1.
            let tv = (((py as f32 + 0.5 - py0) / span_y) * th as f32) as usize;
            let tv = tv.min(th - 1);
            for px in ix0..ix1 {
                let tu = (((px as f32 + 0.5 - px0) / span_x) * tw as f32) as usize;
                let tu = tu.min(tw - 1);
                let texel = frame.pixels[tv * tw + tu];
                if texel == 255 {
                    continue; // transparent
                }
                let idx = py * w + px;
                if vz < zbuf[idx] {
                    image.rgb[idx] = palette[texel as usize];
                    zbuf[idx] = vz;
                }
            }
        }
    }
}

/// Select a sprite's active [`crate::spr::SpriteFrame`] for top-level `frame` index
/// and game `time`, porting `R_GetSpriteframe`: an out-of-range index clamps to 0; a
/// `Group` picks the sub-frame by `targettime = time mod intervals[last]` (first
/// interval strictly greater than targettime).
fn select_sprite_frame(
    sprite: &crate::spr::Sprite,
    frame: usize,
    time: f32,
) -> Option<&crate::spr::SpriteFrame> {
    let f = sprite.frames.get(frame).or_else(|| sprite.frames.first())?;
    match f {
        crate::spr::Frame::Single(sf) => Some(sf),
        crate::spr::Frame::Group { intervals, frames } => {
            let full = intervals.last().copied().unwrap_or(0.0);
            let i = if full > 0.0 && full.is_finite() {
                let targettime = time - (time / full).floor() * full;
                intervals.iter().position(|&iv| iv > targettime).unwrap_or(0)
            } else {
                0
            };
            frames.get(i).or_else(|| frames.first())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a trivial single-frame sprite: `w`x`h` pixels all set to `fill`, with a
    /// centred origin so the billboard straddles the projected point.
    #[cfg(test)]
    fn test_sprite(wpx: i32, hpx: i32, fill: u8) -> crate::spr::Sprite {
        use crate::spr::{Frame, Sprite, SpriteFrame, SpriteHeader};
        Sprite {
            header: SpriteHeader {
                ident: 0, version: 1, type_: 0, boundingradius: 0.0,
                width: wpx, height: hpx, numframes: 1, beamlength: 0.0, synctype: 0,
            },
            frames: vec![Frame::Single(SpriteFrame {
                origin: [-wpx / 2, hpx / 2], // centred
                width: wpx, height: hpx,
                pixels: vec![fill; (wpx * hpx) as usize],
            })],
        }
    }

    #[test]
    fn draw_sprite_in_front_paints_and_z_writes() {
        // A facing sprite 100 units ahead projects near centre and paints its colour.
        let (w, h) = (80usize, 60usize);
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let bg = [9u8, 9, 9];
        let mut img = Image::new(w, h, bg);
        let mut zbuf = vec![f32::INFINITY; w * h];
        let mut pal = [[0u8, 0, 0]; 256];
        pal[42] = [200, 50, 30];
        let spr = test_sprite(16, 16, 42);
        let inst = SpriteInstance { sprite: &spr, origin: [100.0, 0.0, 0.0], frame: 0 };
        draw_sprites(&mut img, &mut zbuf, &cam, std::slice::from_ref(&inst), &pal, 0.0, w, h);
        let painted = img.rgb.iter().filter(|&&p| p == [200, 50, 30]).count();
        assert!(painted > 0, "a sprite in front must paint pixels");
        let nearest = zbuf.iter().cloned().fold(f32::INFINITY, f32::min);
        assert!((nearest - 100.0).abs() < 1.0, "z holds the sprite depth, got {nearest}");
    }

    #[test]
    fn draw_sprite_index_255_is_transparent() {
        // An all-255 sprite is fully transparent: nothing is painted.
        let (w, h) = (80usize, 60usize);
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let bg = [9u8, 9, 9];
        let mut img = Image::new(w, h, bg);
        let mut zbuf = vec![f32::INFINITY; w * h];
        let pal = [[7u8, 7, 7]; 256];
        let spr = test_sprite(16, 16, 255);
        let inst = SpriteInstance { sprite: &spr, origin: [100.0, 0.0, 0.0], frame: 0 };
        draw_sprites(&mut img, &mut zbuf, &cam, std::slice::from_ref(&inst), &pal, 0.0, w, h);
        assert!(img.rgb.iter().all(|&p| p == bg), "index-255 texels are transparent (nothing painted)");
        assert!(zbuf.iter().all(|&z| z == f32::INFINITY), "transparent sprite writes no depth");
    }

    #[test]
    fn draw_sprite_behind_wall_is_z_tested_out() {
        // A sprite at depth 100 behind a wall (z-buffer pre-filled to 10) is hidden.
        let (w, h) = (80usize, 60usize);
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let bg = [9u8, 9, 9];
        let mut img = Image::new(w, h, bg);
        let mut zbuf = vec![10.0f32; w * h];
        let mut pal = [[0u8, 0, 0]; 256];
        pal[42] = [200, 50, 30];
        let spr = test_sprite(16, 16, 42);
        let inst = SpriteInstance { sprite: &spr, origin: [100.0, 0.0, 0.0], frame: 0 };
        draw_sprites(&mut img, &mut zbuf, &cam, std::slice::from_ref(&inst), &pal, 0.0, w, h);
        assert!(img.rgb.iter().all(|&p| p == bg), "a sprite behind a nearer wall is z-tested out");
    }
}
