//! Sprite (SPR) entities.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/r_sprite.c` (`R_DrawSprite`, `R_GetSpriteframe`) and
//! `WinQuake/d_sprite.c`.

use crate::math::{dot, sub, Vec3};
use super::band::Band;
use super::{Frame, Projection};

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
/// texel = 1 world unit) spans `xscale*W/vz` × `yscale*H/vz` framebuffer pixels around
/// the projected origin, offset by the frame's `origin` (left/up). Palette index 255
/// is transparent (`d_sprite.c`). Nearest-neighbour sampled; each pixel takes the
/// sprite spans' test against id's 16-bit z-buffer (`*pz <= izi >> 16`) and writes
/// its 1/z, so nearer geometry wins.
/// Oriented sprites fall back to the facing billboard (good enough for shareware).
// Mirrors R_DrawSprite (r_sprite.c); the C reads globals (vid, r_refdef, cl.time)
// that this port reads from the frame.
pub(super) fn draw_sprites(band: &mut Band, frame: &Frame) {
    const NEAR: f32 = 1.0;
    let (cam, scene, w, h) = (&frame.cam, frame.scene, frame.w, frame.h);
    let (opts, sprites, palette, time) = (&scene.options, scene.sprites, scene.palette, scene.time);
    if w == 0 || h == 0 || sprites.is_empty() {
        return;
    }
    let (forward, right, up) = cam.basis();
    // R_SetupAndDrawSprite projects the sprite's corners with xscale/yscale, so
    // a sprite is `pixelAspect` as tall in pixels as it is wide.
    let Projection { cx, cy, xscale: focal, yscale } = Projection::new(cam, w, h, opts.aspect());

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
        let sy = cy - yscale * dot(rel, up) / vz;
        // The billboard faces the eye, so its 1/z is the same at every pixel:
        // `izi = (int)(zi * 0x8000 * 0x10000)`, compared as `izi >> 16`.
        let izi16 = ((1.0 / vz) * 32768.0 * 65536.0) as i32 >> 16;
        // 1 texel = 1 world unit; the facing billboard scales by xscale/vz across
        // and yscale/vz down. The frame `origin` is the left/up offset of its
        // top-left from the centre (Quake: up = origin[1], down =
        // origin[1]-height, left = origin[0]).
        let scale = focal / vz;
        let yscale_z = yscale / vz;
        let (fw, fh) = (frame.width as f32, frame.height as f32);
        let (ox, oy) = (frame.origin[0] as f32, frame.origin[1] as f32);
        // +up is -screen-y; the top edge is at world up-offset `oy`.
        let x0 = sx + ox * scale;
        let x1 = sx + (ox + fw) * scale;
        let y0 = sy - oy * yscale_z;
        let y1 = sy - (oy - fh) * yscale_z;
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
        // Only the rows of the band (a band draws its share of the sprite).
        let rows = band.rows();
        for py in iy0.max(rows.start)..iy1.min(rows.end) {
            // Texel row: fraction down the screen rect -> 0..th-1.
            let tv = (((py as f32 + 0.5 - py0) / span_y) * th as f32) as usize;
            let tv = tv.min(th - 1);
            let Some((prow, zrow)) = band.span(ix0, py, ix1.saturating_sub(ix0)) else { continue };
            for (k, (p, z)) in prow.iter_mut().zip(zrow.iter_mut()).enumerate() {
                let px = ix0 + k;
                let tu = (((px as f32 + 0.5 - px0) / span_x) * tw as f32) as usize;
                let tu = tu.min(tw - 1);
                let texel = frame.pixels[tv * tw + tu];
                if texel == 255 {
                    continue; // transparent
                }
                // D_SpriteDrawSpans: `if (*pz <= (izi >> 16)) *pz = izi >> 16`.
                if *z as i32 <= izi16 {
                    *z = izi16 as i16;
                    *p = palette[texel as usize];
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
    use crate::render::fixtures::test_sprite;
    use crate::render::{Camera, Image, Palette, Scene};

    /// `draw_sprites` for one sprite seen by `cam`, into `img` and `zbuf`.
    fn draw(img: &mut Image, zbuf: &mut [i16], cam: Camera, inst: &SpriteInstance, pal: &Palette) {
        let world = crate::render::demo_room();
        let scene = Scene { sprites: std::slice::from_ref(inst), ..Scene::new(&world, cam, img.w, img.h, pal) };
        let frame = Frame::new(&scene, img.w, img.h);
        draw_sprites(&mut Band::whole(img.w, &mut img.rgb, zbuf), &frame);
    }

    #[test]
    fn draw_sprite_in_front_paints_and_z_writes() {
        // A facing sprite 100 units ahead projects near centre and paints its colour.
        let (w, h) = (80usize, 60usize);
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let bg = [9u8, 9, 9];
        let mut img = Image::new(w, h, bg);
        let mut zbuf = vec![i16::MIN; w * h];
        let mut pal = [[0u8, 0, 0]; 256];
        pal[42] = [200, 50, 30];
        let spr = test_sprite(16, 16, 42);
        let inst = SpriteInstance { sprite: &spr, origin: [100.0, 0.0, 0.0], frame: 0 };
        draw(&mut img, &mut zbuf, cam, &inst, &pal);
        let painted = img.rgb.iter().filter(|&&p| p == [200, 50, 30]).count();
        assert!(painted > 0, "a sprite in front must paint pixels");
        // (int)(1/100 * 0x8000 * 0x10000) >> 16 = 327.
        assert_eq!(zbuf.iter().copied().max().unwrap(), 327, "z holds the sprite's 1/z");
    }

    #[test]
    fn draw_sprite_index_255_is_transparent() {
        // An all-255 sprite is fully transparent: nothing is painted.
        let (w, h) = (80usize, 60usize);
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let bg = [9u8, 9, 9];
        let mut img = Image::new(w, h, bg);
        let mut zbuf = vec![i16::MIN; w * h];
        let pal = [[7u8, 7, 7]; 256];
        let spr = test_sprite(16, 16, 255);
        let inst = SpriteInstance { sprite: &spr, origin: [100.0, 0.0, 0.0], frame: 0 };
        draw(&mut img, &mut zbuf, cam, &inst, &pal);
        assert!(img.rgb.iter().all(|&p| p == bg), "index-255 texels are transparent (nothing painted)");
        assert!(zbuf.iter().all(|&z| z == i16::MIN), "transparent sprite writes no depth");
    }

    #[test]
    fn draw_sprite_behind_wall_is_z_tested_out() {
        // A sprite at depth 100 behind a wall (the z-buffer holding depth 10's 1/z) is hidden.
        let (w, h) = (80usize, 60usize);
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let bg = [9u8, 9, 9];
        let mut img = Image::new(w, h, bg);
        let mut zbuf = vec![3276i16; w * h];
        let mut pal = [[0u8, 0, 0]; 256];
        pal[42] = [200, 50, 30];
        let spr = test_sprite(16, 16, 42);
        let inst = SpriteInstance { sprite: &spr, origin: [100.0, 0.0, 0.0], frame: 0 };
        draw(&mut img, &mut zbuf, cam, &inst, &pal);
        assert!(img.rgb.iter().all(|&p| p == bg), "a sprite behind a nearer wall is z-tested out");
    }
}
