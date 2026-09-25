//! Drawing particles into the 3-D view.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/r_part.c` — `R_DrawParticles` (`D_DrawParticle`, `d_part.c`).
//! The particle simulation itself is [`crate::particles`].

use crate::math::{dot, sub, Vec3};
use super::{Camera, Image};

/// Draw a set of engine particles into `image`, z-tested and depth-written
/// against the shared `zbuf`: id's `D_DrawParticle` (`d_part.c`, the portable
/// C of `d_parta.s`) with `R_DrawParticles`' projection (`r_part.c`) and the
/// constants `D_ViewChanged` derives from the view (`d_modech.c`).
///
/// Each particle is `(world_pos, palette index)`, drawn in slice order (a
/// later one wins a tie, as id's list order does). `w`/`h` is the view,
/// `r_refdef.vrect` (the image is the view alone, so `vrect.x = vrect.y = 0`).
/// Per particle, as the C:
/// - `transformed = (local·vright*xscaleshrink, local·vup*yscaleshrink,
///   local·vpn)` with `local = p - r_origin` (`R_DrawParticles` scales
///   `r_pright`/`r_pup` by the SHRUNK scales, `R_ViewChanged`:
///   `xscaleshrink = (vrect.width-6)/horizontalFieldOfView`, `yscaleshrink =
///   xscaleshrink*pixelAspect` — 3 px nearer the centre at the edge of a
///   320-wide view than the walls' `xscale`); dropped when `transformed[2] <
///   PARTICLE_Z_CLIP` (8).
/// - `zi = 1/transformed[2]`, `u = (int)(xcenter + zi*transformed[0] + 0.5)`,
///   `v = (int)(ycenter - zi*transformed[1] + 0.5)` with `xcenter =
///   vrect.width/2 - 0.5` (`XCENTERING`); the whole particle is dropped unless
///   `vrecty <= v <= d_vrectbottom_particle` and `vrectx <= u <=
///   d_vrectright_particle` (the view less `d_pix_max`, so a square never
///   crosses the edge).
/// - `izi = (int)(zi*0x8000)`, `pix = izi >> d_pix_shift` clamped to
///   `[d_pix_min, d_pix_max]` (`d_pix_min = max(1, width/320)`, `d_pix_max =
///   (int)(width/80 + 0.5)`, `d_pix_shift = 8 - (int)(width/320 + 0.5)`: at
///   320 wide a particle is `256/z` pixels, 1 to 4).
/// - a `pix` wide, `pix << d_y_aspect_shift` tall block from `(u, v)` right
///   and down (`d_y_aspect_shift` = 1 only when `pixelAspect > 1.4`), each
///   pixel written where `pz <= izi` — `zbuf` is id's 16-bit `d_pzbuffer` as
///   the edge renderer's `D_DrawZSpans` and the models left it, the short
///   promoted to `int` for the compare — and `izi` stored, truncated to the
///   short. Particles of a burst often share an `izi`; the later one in the
///   list wins those ties, as in the C.
///
/// Degenerate sizes and non-finite projections draw nothing.
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
    /// `d_iface.h`: particles nearer than this are not drawn.
    const PARTICLE_Z_CLIP: f32 = 8.0;
    if w == 0 || h == 0 || particles.is_empty() {
        return;
    }

    let (forward, right, up) = cam.basis();
    let proj = ParticleProjection::new(cam, w, h, pixel_aspect);
    // R_DrawParticles: r_pright = vright*xscaleshrink, r_pup = vup*yscaleshrink.
    let pright = [right[0] * proj.xscaleshrink, right[1] * proj.xscaleshrink, right[2] * proj.xscaleshrink];
    let pup = [up[0] * proj.yscaleshrink, up[1] * proj.yscaleshrink, up[2] * proj.yscaleshrink];
    let rows_shift = proj.y_aspect_shift;

    for &(p, color) in particles {
        let local = sub(p, cam.pos);
        let t = [dot(local, pright), dot(local, pup), dot(local, forward)];
        if t[2] < PARTICLE_Z_CLIP {
            continue; // (a NaN depth fails the finite test below)
        }
        let zi = 1.0 / t[2];
        let fu = proj.xcenter + zi * t[0] + 0.5;
        let fv = proj.ycenter - zi * t[1] + 0.5;
        if !(fu.is_finite() && fv.is_finite()) {
            continue;
        }
        // (int) truncates toward zero, as `as` does for in-range values.
        let (u, v) = (fu as i64, fv as i64);
        if v > proj.vrectbottom_particle || u > proj.vrectright_particle || v < 0 || u < 0 {
            continue;
        }
        let izi = zbuf_izi(zi);
        let pix = (izi >> proj.pix_shift).clamp(proj.pix_min, proj.pix_max);
        let rgb = palette[color as usize];
        for row in 0..(pix << rows_shift) {
            let base = (v + row) as usize * w + u as usize;
            for i in 0..pix as usize {
                let idx = base + i;
                if let (Some(z), Some(dst)) = (zbuf.get_mut(idx), image.rgb.get_mut(idx)) {
                    // if (pz[i] <= izi) { pz[i] = izi; pdest[i] = color; }
                    // (id's d_pzbuffer: 16-bit, the short promoted to int
                    // for the compare and truncated on the store.)
                    if *z as i64 <= izi {
                        *z = izi as i16;
                        *dst = rgb;
                    }
                }
            }
        }
    }
}

/// `izi = (int)(zi * 0x8000)`: `D_DrawParticle`'s 1/z as its z-buffer holds
/// it in id's 16-bit `d_pzbuffer` (the edge renderer's; an empty pixel holds 0).
fn zbuf_izi(zi: f32) -> i64 {
    (zi * 32768.0) as i64
}

/// What `R_ViewChanged` (`r_main.c`) and `D_ViewChanged` (`d_modech.c`)
/// derive from the view for `D_DrawParticle`, for a `w x h` view at the
/// origin of its own image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ParticleProjection {
    /// `xcenter = vrect.width*XCENTERING + vrect.x - 0.5`, likewise `ycenter`.
    pub xcenter: f32,
    pub ycenter: f32,
    /// `(vrect.width-6) / horizontalFieldOfView`, and that times `pixelAspect`.
    pub xscaleshrink: f32,
    pub yscaleshrink: f32,
    pub pix_min: i64,
    pub pix_max: i64,
    pub pix_shift: u32,
    pub y_aspect_shift: u32,
    /// `vrectright - d_pix_max`, `vrectbottom - (d_pix_max << d_y_aspect_shift)`:
    /// the last column/row a particle may start on.
    pub vrectright_particle: i64,
    pub vrectbottom_particle: i64,
}

impl ParticleProjection {
    pub(crate) fn new(cam: &Camera, w: usize, h: usize, pixel_aspect: f32) -> Self {
        let (wf, hf) = (w as f32, h as f32);
        // horizontalFieldOfView = 2*tan(fov_x/2), a float; a degenerate fov
        // falls back to ~90 degrees as `Projection` does.
        let hfov = (2.0 * (cam.fov_deg as f64 * 0.5).to_radians().tan()) as f32;
        let hfov = if hfov.abs() < 2e-6 { 2.0 } else { hfov };
        let xscaleshrink = (w as i64 - 6) as f32 / hfov;
        let pix_min = (w as i64 / 320).max(1);
        let pix_max = ((wf / 80.0 + 0.5) as i64).max(1);
        // d_pix_shift = 8 - (int)(width/320 + 0.5); negative (a view over
        // 2720 wide) would be undefined in the C.
        let pix_shift = (8 - (wf / 320.0 + 0.5) as i64).max(0) as u32;
        let y_aspect_shift = u32::from(pixel_aspect > 1.4);
        ParticleProjection {
            xcenter: wf * 0.5 - 0.5,
            ycenter: hf * 0.5 - 0.5,
            xscaleshrink,
            yscaleshrink: xscaleshrink * pixel_aspect,
            pix_min,
            pix_max,
            pix_shift,
            y_aspect_shift,
            vrectright_particle: w as i64 - pix_max,
            vrectbottom_particle: h as i64 - (pix_max << y_aspect_shift),
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
        // particle 30 units above the axis at depth 100 sits 157*30/100 = 47.1
        // rows above ycenter 99.5 with square pixels (v = (int)52.9), 39.25 at
        // id's 4:3 aspect 0.8333 (v = (int)60.75).
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

    /// Where and how big `draw_particles` draws one particle alone: the
    /// painted pixels' bounding box `(x0, y0, x1, y1)` inclusive, or None.
    fn particle_box(w: usize, h: usize, p: Vec3, aspect: f32) -> Option<(usize, usize, usize, usize)> {
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let mut pal = [[0u8, 0, 0]; 256];
        pal[42] = [200, 50, 30];
        let mut img = Image::new(w, h, [0, 0, 0]);
        let mut zbuf = vec![0i16; w * h]; // id's d_pzbuffer: 0 = nothing nearer
        draw_particles(&mut img, &mut zbuf, &cam, &[(p, 42)], &pal, w, h, aspect);
        let px: Vec<(usize, usize)> =
            (0..w * h).filter(|&i| img.rgb[i] == [200, 50, 30]).map(|i| (i % w, i / w)).collect();
        let (x0, y0) = (px.iter().map(|p| p.0).min()?, px.iter().map(|p| p.1).min()?);
        let (x1, y1) = (px.iter().map(|p| p.0).max()?, px.iter().map(|p| p.1).max()?);
        assert_eq!(px.len(), (x1 - x0 + 1) * (y1 - y0 + 1), "a particle is a solid block");
        Some((x0, y0, x1, y1))
    }

    #[test]
    fn particles_project_with_r_main_c_xscaleshrink() {
        // R_ViewChanged: xscaleshrink = (vrect.width-6)/horizontalFieldOfView,
        // 314/2 = 157 at 320 wide and fov 90 (the walls' xscale is 160).
        // D_DrawParticle: u = (int)(xcenter + zi*x*xscaleshrink + 0.5), xcenter
        // 159.5. A particle 95 units right at depth 100: 159.5 + 149.15 + 0.5
        // -> column 309 (xscale would give 312); 95 up: 99.5 - 149.15 + 0.5 <
        // 0, dropped; 60 up: 99.5 - 94.2 + 0.5 -> row 5 (xscale: 4).
        let right = [100.0, -95.0, 0.0];
        assert_eq!(particle_box(320, 200, right, 1.0).map(|b| (b.0, b.1)), Some((309, 100)));
        let left = [100.0, 95.0, 0.0];
        assert_eq!(particle_box(320, 200, left, 1.0).map(|b| (b.0, b.1)), Some((10, 100)));
        assert_eq!(particle_box(320, 200, [100.0, 0.0, 60.0], 1.0).map(|b| (b.0, b.1)), Some((160, 5)));
        assert_eq!(particle_box(320, 200, [100.0, 0.0, 95.0], 1.0), None);
        let p = ParticleProjection::new(
            &Camera { pos: [0.0; 3], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 }, 320, 200, 0.8333333);
        assert!((p.xscaleshrink - 157.0).abs() < 1e-3 && (p.yscaleshrink - 157.0 * 0.8333333).abs() < 1e-3);
        assert_eq!((p.xcenter, p.ycenter), (159.5, 99.5));
    }

    #[test]
    fn particle_size_is_d_part_cs_izi_shift() {
        // pix = ((int)(zi*0x8000) >> d_pix_shift) clamped to [d_pix_min,
        // d_pix_max], drawn from (u, v) right and down. 320 wide: shift 7,
        // [1, 4]: depth 100 -> 327>>7 = 2; 64 -> 4; 30 -> 8, clamped to 4; 300
        // -> 0, clamped to 1.
        let at = |w: usize, h: usize, z: f32| {
            particle_box(w, h, [z, 0.0, 0.0], 1.0).map(|(x0, y0, x1, y1)| (x0, y0, x1 - x0 + 1, y1 - y0 + 1))
        };
        assert_eq!(at(320, 200, 100.0), Some((160, 100, 2, 2)));
        assert_eq!(at(320, 200, 64.0), Some((160, 100, 4, 4)));
        assert_eq!(at(320, 200, 30.0), Some((160, 100, 4, 4)));
        assert_eq!(at(320, 200, 300.0), Some((160, 100, 1, 1)));
        // 640 wide: shift 6, [2, 8]: depth 100 -> 327>>6 = 5; 1000 -> 0 -> 2.
        assert_eq!(at(640, 400, 100.0), Some((320, 200, 5, 5)));
        assert_eq!(at(640, 400, 1000.0), Some((320, 200, 2, 2)));
        // 960 wide: shift 5, [3, 12].
        assert_eq!(at(960, 600, 100.0), Some((480, 300, 10, 10)));
    }

    #[test]
    fn particles_obey_d_part_cs_clip_and_edges() {
        // PARTICLE_Z_CLIP: nothing nearer than 8 units.
        assert_eq!(particle_box(320, 200, [7.9, 0.0, 0.0], 1.0), None);
        assert!(particle_box(320, 200, [8.0, 0.0, 0.0], 1.0).is_some());
        // d_vrectright_particle = 320 - d_pix_max (4): a particle starting on
        // column 317 is dropped whole, not clipped to the edge; 316 is drawn.
        // Column u = (int)(160 + 1.57*y): y = 100 -> 317; y = 99 -> 315.43 -> 315.
        assert_eq!(particle_box(320, 200, [100.0, -100.0, 0.0], 1.0), None);
        assert_eq!(particle_box(320, 200, [100.0, -99.0, 0.0], 1.0).map(|b| b.0), Some(315));
        // pixelAspect over 1.4 doubles the rows (d_y_aspect_shift).
        let tall = particle_box(320, 200, [100.0, 0.0, 0.0], 1.5).unwrap();
        assert_eq!((tall.2 - tall.0 + 1, tall.3 - tall.1 + 1), (2, 4));
    }

    #[test]
    fn particles_tie_on_d_part_cs_quantized_1_over_z() {
        // Two particles on one pixel whose izi = (int)(0x8000/z) is equal
        // (depths 150 and 150.1: both 218): the later one in the list wins, as
        // id's `pz <= izi`; one nearer by a whole izi step wins from anywhere.
        let (w, h) = (320usize, 200usize);
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let mut pal = [[0u8, 0, 0]; 256];
        pal[1] = [255, 0, 0];
        pal[2] = [0, 255, 0];
        let centre = |parts: &[(Vec3, u8)]| {
            let mut img = Image::new(w, h, [0, 0, 0]);
            let mut zbuf = vec![0i16; w * h]; // id's d_pzbuffer: 0 = nothing nearer
            draw_particles(&mut img, &mut zbuf, &cam, parts, &pal, w, h, 1.0);
            img.rgb[100 * w + 160]
        };
        assert_eq!((zbuf_izi(1.0 / 150.0), zbuf_izi(1.0 / 150.1)), (218, 218));
        assert_eq!(centre(&[([150.0, 0.0, 0.0], 1), ([150.1, 0.0, 0.0], 2)]), [0, 255, 0]);
        assert_eq!(centre(&[([150.1, 0.0, 0.0], 1), ([150.0, 0.0, 0.0], 2)]), [0, 255, 0]);
        assert_eq!(centre(&[([140.0, 0.0, 0.0], 1), ([150.0, 0.0, 0.0], 2)]), [255, 0, 0]);
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
