//! Drawing particles into the 3-D view.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/r_part.c` — `R_DrawParticles` (`D_DrawParticle`, `d_part.c`).
//! The particle simulation itself is [`crate::particles`].

use crate::math::{dot, sub, Vec3};
use super::{Camera, Image, Projection};

/// Draw a set of engine particles into `image`, z-tested and depth-written
/// against the shared `zbuf`, porting the visible result of Quake's software
/// `R_DrawParticles` (`d_*.c`).
///
/// Each particle is `(world_pos, palette index)`. The projection matches every
/// other pass in this module (and [`render_scene_ext`](super::render_scene_ext), whose buffer this shares):
/// `rel = p - cam.pos`; the forward depth `vz = dot(rel, forward)` is the z-test
/// key; a particle at or behind the near plane (`vz <= NEAR`) is skipped; the
/// screen position is `sx = cx + xscale*dot(rel,right)/vz`,
/// `sy = cy - yscale*dot(rel,up)/vz` ([`Projection`](super::Projection):
/// `yscale = xscale * pixel_aspect`, as `R_DrawParticles`' `r_pup` is `vup`
/// scaled by `yscaleshrink`). The square stays square in pixels whatever the
/// aspect, as `D_DrawParticle`'s is below `pixelAspect` 1.4.
///
/// A particle is drawn as a `pix`x`pix` filled square whose side scales
/// **continuously** with `1/z`, porting `R_DrawParticles`/`D_DrawParticle`
/// (`d_part.c`): the C computes `izi = zi*0x8000` (`zi = 1/z`),
/// `pix = izi >> d_pix_shift`, then clamps to `[d_pix_min, d_pix_max]`. The
/// resolution-derived constants are `d_pix_min = max(1, width/320)`,
/// `d_pix_max = round(width/80)`, `d_pix_shift = 8 - round(width/320)`
/// (`d_modech.c`). We reproduce that same continuous ramp (rather than a
/// 2-bucket step) so a particle grows smoothly as it nears the eye and shrinks to
/// the minimum size far away. Every covered pixel takes `D_DrawParticle`'s test
/// against id's 16-bit z-buffer: `izi = (int)(zi * 0x8000)`, drawn where
/// `pz <= izi`, which it writes.
/// Off-screen pixels are clipped by the loop bounds; the colour is
/// `palette[color]`.
///
/// SAFETY: `w`/`h` of `0`, non-finite projections, and out-of-range indices are
/// all guarded; the only direct indexing is into the freshly-sized framebuffers,
/// where the index is provably in bounds.
#[allow(clippy::too_many_arguments)]
pub fn draw_particles(
    image: &mut Image,
    zbuf: &mut [i16],
    cam: &Camera,
    particles: &[(Vec3, u8)],
    palette: &[[u8; 3]; 256],
    w: usize,
    h: usize,
    pixel_aspect: f32,
) {
    const NEAR: f32 = 1.0;
    if w == 0 || h == 0 || particles.is_empty() {
        return;
    }

    let (forward, right, up) = cam.basis();
    let Projection { cx, cy, xscale: focal, yscale } = Projection::new(cam, w, h, pixel_aspect);

    // Resolution-scaled particle-size clamp, ported from `D_DrawParticle` /
    // `d_modech.c`. Quake authored its `0x8000`/`d_pix_shift` ramp against a
    // 320-wide virtual screen; at a render width `w` the bounds scale the same
    // way: `d_pix_min = max(1, w/320)`, `d_pix_max = round(w/80)`. We size the
    // continuous ramp from the projected world extent (`focal/vz`) — exactly the
    // `zi`-proportional growth the C produced — and clamp to those bounds.
    let d_pix_min: i64 = ((w as f32 / 320.0) as i64).max(1);
    let d_pix_max: i64 = (w as f32 / 80.0 + 0.5).floor() as i64;
    let d_pix_max = d_pix_max.max(d_pix_min);

    for &(p, color) in particles {
        let rel = sub(p, cam.pos);
        let vz = dot(rel, forward);
        if vz <= NEAR {
            // At/behind the near plane: skip (matches the world/model near clip).
            continue;
        }
        let vx = dot(rel, right);
        let vy = dot(rel, up);
        let sx = cx + focal * vx / vz;
        let sy = cy - yscale * vy / vz;
        if !(sx.is_finite() && sy.is_finite()) {
            continue;
        }

        // Continuous 1/z size ramp (D_DrawParticle): the projected on-screen size
        // of a ~1-unit particle is `focal/vz`; this grows smoothly as the particle
        // nears the eye. Clamp to the resolution-scaled `[d_pix_min, d_pix_max]`.
        let pix = (focal / vz).round() as i64;
        let pix = pix.clamp(d_pix_min, d_pix_max);

        let rgb = palette[color as usize];
        // D_DrawParticle's 16-bit 1/z: `zi = 1.0 / transformed[2]`, `izi =
        // (int)(zi * 0x8000)`.
        let izi = ((1.0 / vz) * 32768.0) as i32;

        // Draw a `pix`x`pix` square. The C anchors the square at `(u,v)` and
        // extends right/down; we centre it on the projected point (`half` each
        // way) so growth stays symmetric about the particle. `half = (pix-1)/2`
        // gives a `pix`-wide span (pix=1 -> single pixel, pix=3 -> 3x3, …).
        let half: i64 = (pix - 1) / 2;

        // Centre pixel + a square around it, each pixel z-tested.
        let cx_px = sx.floor() as i64;
        let cy_px = sy.floor() as i64;
        for py in (cy_px - half)..=(cy_px + half) {
            if py < 0 || py >= h as i64 {
                continue;
            }
            for px in (cx_px - half)..=(cx_px + half) {
                if px < 0 || px >= w as i64 {
                    continue;
                }
                let idx = (py as usize) * w + (px as usize);
                if let Some(z) = zbuf.get_mut(idx) {
                    if *z as i32 <= izi {
                        *z = izi as i16;
                        if let Some(dst) = image.rgb.get_mut(idx) {
                            *dst = rgb;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{demo_room, render_scene, render_scene_ext};
    use crate::render::light::NEUTRAL_LIGHTSTYLE_SCALES;

    // -- Engine particles (draw_particles: projection + z-test) ---------------

    #[test]
    fn draw_particles_in_front_changes_a_pixel() {
        // A camera at the origin looking down +X; a particle 100 units straight
        // ahead must project near screen centre and paint its palette colour over
        // a z-buffer that holds nothing nearer.
        let w = 80usize;
        let h = 60usize;
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let bg = [9u8, 9, 9];
        let mut img = Image::new(w, h, bg);
        let mut zbuf = vec![i16::MIN; w * h];
        let mut pal = [[0u8, 0, 0]; 256];
        pal[42] = [200, 50, 30]; // the particle colour

        draw_particles(&mut img, &mut zbuf, &cam, &[([100.0, 0.0, 0.0], 42)], &pal, w, h, 1.0);

        // Some pixel changed to the particle colour, and the matching z-buffer
        // slot now holds the particle's 1/z: (int)(0x8000 / 100) = 327.
        let painted = img.rgb.iter().filter(|&&p| p == [200, 50, 30]).count();
        assert!(painted > 0, "a particle in front must paint at least one pixel");
        let nearest = zbuf.iter().copied().max().unwrap();
        assert_eq!(nearest, 327, "z-buffer holds the particle's 1/z");
    }

    #[test]
    fn draw_particles_places_rows_by_the_pixel_aspect() {
        // R_DrawParticles projects with r_pup = vup * yscaleshrink: at 320x200 a
        // particle 30 units above the axis at depth 100 sits 160*30/100 = 48 rows
        // above the centre with square pixels, 40 at id's 4:3 aspect 0.8333.
        let (w, h) = (320usize, 200usize);
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let mut pal = [[0u8, 0, 0]; 256];
        pal[42] = [200, 50, 30];
        let row_of = |aspect: f32| {
            let mut img = Image::new(w, h, [0, 0, 0]);
            let mut zbuf = vec![i16::MIN; w * h];
            draw_particles(&mut img, &mut zbuf, &cam, &[([100.0, 0.0, 30.0], 42)], &pal, w, h, aspect);
            let i = img.rgb.iter().position(|&p| p == [200, 50, 30]).expect("particle drawn");
            (i % w, i / w)
        };
        let (x1, y1) = row_of(1.0);
        let (x2, y2) = row_of(200.0 / 320.0 * 4.0 / 3.0);
        assert_eq!(x1, x2, "the aspect never moves a particle sideways");
        assert_eq!((y1, y2), (52, 60));
    }

    #[test]
    fn draw_particles_behind_wall_is_z_tested_out() {
        // Same view, but pre-fill the z-buffer with a NEARER 1/z (a wall at
        // depth 10, 0x8000/10) everywhere. A particle at depth 100 is behind it
        // and must NOT be drawn (D_DrawParticle draws only where pz <= izi).
        let w = 80usize;
        let h = 60usize;
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let bg = [9u8, 9, 9];
        let mut img = Image::new(w, h, bg);
        let mut zbuf = vec![3276i16; w * h]; // a wall closer than the particle
        let mut pal = [[0u8, 0, 0]; 256];
        pal[42] = [200, 50, 30];

        draw_particles(&mut img, &mut zbuf, &cam, &[([100.0, 0.0, 0.0], 42)], &pal, w, h, 1.0);

        // Nothing painted: the wall occludes the particle.
        assert!(
            img.rgb.iter().all(|&p| p == bg),
            "a particle behind a nearer wall must be z-tested out (not drawn)"
        );
        // And the z-buffer is unchanged (still the wall depth).
        assert!(zbuf.iter().all(|&z| z == 3276), "occluded particle must not overwrite the z-buffer");
    }

    #[test]
    fn draw_particles_in_front_overwrites_farther_wall() {
        // A particle CLOSER than the existing z-buffer (a wall at depth 500) must
        // win the z-test and paint, writing its own depth — the complement of the
        // occlusion test above.
        let w = 80usize;
        let h = 60usize;
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let mut img = Image::new(w, h, [9, 9, 9]);
        let mut zbuf = vec![65i16; w * h]; // a wall FARTHER than the particle (depth 500)
        let mut pal = [[0u8, 0, 0]; 256];
        pal[7] = [10, 220, 40];

        draw_particles(&mut img, &mut zbuf, &cam, &[([100.0, 0.0, 0.0], 7)], &pal, w, h, 1.0);

        let painted = img.rgb.iter().filter(|&&p| p == [10, 220, 40]).count();
        assert!(painted > 0, "a particle nearer than the wall must paint");
        let nearest = zbuf.iter().copied().max().unwrap();
        assert_eq!(nearest, 327, "nearer particle writes its 1/z");
    }

    #[test]
    fn draw_particles_behind_camera_is_skipped() {
        // A particle at/behind the near plane (here directly behind the camera)
        // must be skipped entirely — no panic, no paint.
        let w = 40usize;
        let h = 30usize;
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let bg = [9u8, 9, 9];
        let mut img = Image::new(w, h, bg);
        let mut zbuf = vec![i16::MIN; w * h];
        let pal = [[200u8, 200, 200]; 256];

        // -X is behind a camera looking down +X.
        draw_particles(&mut img, &mut zbuf, &cam, &[([-100.0, 0.0, 0.0], 0)], &pal, w, h, 1.0);
        assert!(img.rgb.iter().all(|&p| p == bg), "a particle behind the camera draws nothing");
    }

    #[test]
    fn render_scene_empty_particles_matches_no_particles() {
        // Passing an empty particle slice to render_scene_ext must reproduce the
        // exact frame render_scene produces (no particles == no change).
        let bsp = demo_room();
        let cam = Camera::looking_at([0.0, 0.0, 0.0], [200.0, 0.0, 0.0], 90.0);
        let pal = [[180u8, 180, 180]; 256];
        let with_empty = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None);
        let baseline = render_scene(&bsp, &cam, 160, 120, &pal, &[]);
        assert_eq!(
            with_empty.rgb, baseline.rgb,
            "render_scene_ext with an empty particle slice must equal render_scene"
        );
    }

    #[test]
    fn render_scene_particles_paint_into_the_world_frame() {
        // A bright particle placed in the empty centre of demo_room (in front of
        // the camera, in clear air before the far wall) must change the rendered
        // frame versus the same scene with no particles.
        let bsp = demo_room();
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let mut pal = [[60u8, 60, 60]; 256];
        pal[251] = [255, 0, 255]; // a vivid colour unlikely to match the walls
        let without = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None);
        // A particle ~80 units in front of the camera (well before the +256 wall).
        let with = render_scene_ext(
            &bsp,
            &cam,
            160,
            120,
            &pal,
            &[],
            &[],
            &[],
            None,
            0.0,
            &[([-120.0, 0.0, 0.0], 251)],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
        assert_ne!(without.rgb, with.rgb, "a visible particle must change the frame");
        assert!(
            with.rgb.contains(&[255, 0, 255]),
            "the particle's palette colour must appear in the frame"
        );
    }
}
