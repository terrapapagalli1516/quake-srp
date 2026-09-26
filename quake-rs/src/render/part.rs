//! Drawing particles into the 3-D view.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/r_part.c` — `R_DrawParticles` (`D_DrawParticle`, `d_part.c`).
//! The particle simulation itself is [`crate::particles`].

use crate::math::{dot, sub, Vec3};
use super::band::Band;
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
pub fn draw_particles(
    image: &mut Image,
    zbuf: &mut [i16],
    cam: &Camera,
    particles: &[(Vec3, u8)],
    w: usize,
    h: usize,
    pixel_aspect: f32,
) {
    let proj = ParticleProjection::new(cam, w, h, pixel_aspect, false);
    let dots = project_particles(cam, &proj, particles);
    let n = w.saturating_mul(h).min(image.pixels.len());
    draw_particle_dots(&mut Band::whole(w, &mut image.pixels[..n], zbuf), &dots);
}

/// One particle as `D_DrawParticle` draws it: a `pix` wide, `rows` tall
/// square from `(u, v)` right and down, at `izi`, in palette index `color`.
#[derive(Clone, Copy, Debug)]
pub(super) struct ParticleDot {
    u: usize,
    v: usize,
    pix: usize,
    rows: usize,
    izi: i64,
    color: u8,
}

/// `R_DrawParticles`' projection of `particles` (see [`draw_particles`]), in
/// list order: the ones `D_DrawParticle` draws, as squares.
pub(super) fn project_particles(
    cam: &Camera,
    proj: &ParticleProjection,
    particles: &[(Vec3, u8)],
) -> Vec<ParticleDot> {
    /// `d_iface.h`: particles nearer than this are not drawn.
    const PARTICLE_Z_CLIP: f32 = 8.0;
    let (forward, right, up) = cam.basis();
    // R_DrawParticles: r_pright = vright*xscaleshrink, r_pup = vup*yscaleshrink.
    let pright = [right[0] * proj.xscaleshrink, right[1] * proj.xscaleshrink, right[2] * proj.xscaleshrink];
    let pup = [up[0] * proj.yscaleshrink, up[1] * proj.yscaleshrink, up[2] * proj.yscaleshrink];
    let mut dots = Vec::with_capacity(particles.len());
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
        let pix = ((izi * proj.pix_mul) >> proj.pix_shift).clamp(proj.pix_min, proj.pix_max);
        dots.push(ParticleDot {
            u: u as usize,
            v: v as usize,
            pix: pix as usize,
            rows: (pix << proj.y_aspect_shift) as usize,
            izi,
            color,
        });
    }
    dots
}

/// `D_DrawParticle` for `dots` in list order, the pixels in `band`'s rows:
/// each pixel written where `pz <= izi` — the z-buffer is id's 16-bit
/// `d_pzbuffer` as the edge renderer's `D_DrawZSpans` and the models left
/// it, the short promoted to `int` for the compare — and `izi` stored,
/// truncated to the short. Particles of a burst often share an `izi`; the
/// later one in the list wins those ties, as in the C.
pub(super) fn draw_particle_dots(band: &mut Band, dots: &[ParticleDot]) {
    let w = band.width();
    let own = band.indices();
    for d in dots {
        for row in 0..d.rows {
            let base = (d.v + row) * w + d.u;
            if base >= own.end || base + d.pix <= own.start {
                continue; // another band's
            }
            for idx in base..base + d.pix {
                if let Some((dst, z)) = band.at(idx) {
                    // if (pz[i] <= izi) { pz[i] = izi; pdest[i] = color; }
                    if *z as i64 <= d.izi {
                        *z = d.izi as i16;
                        *dst = d.color;
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
    /// A particle is `(izi * pix_mul) >> pix_shift` pixels (then clamped):
    /// id's `izi >> d_pix_shift` (`pix_mul` 1), or the hires extra's
    /// proportional size in 16.16 ([`ParticleProjection::new`]).
    pub pix_mul: i64,
    pub pix_shift: u32,
    pub y_aspect_shift: u32,
    /// `vrectright - d_pix_max`, `vrectbottom - (d_pix_max << d_y_aspect_shift)`:
    /// the last column/row a particle may start on.
    pub vrectright_particle: i64,
    pub vrectbottom_particle: i64,
}

impl ParticleProjection {
    /// `R_ViewChanged`'s and `D_ViewChanged`'s particle state for a `w x h`
    /// view. `hires` (the hires extra, not id) sizes particles in proportion
    /// to the view's scale instead of by id's `d_pix_*`.
    ///
    /// id's size is `izi >> d_pix_shift` with `d_pix_shift = 8 -
    /// (int)(width/320 + 0.5)`: a halving per 320 pixels of width where the
    /// width only adds 320 each time, so it is proportional only at 320 and
    /// 640 wide — at 1280 a particle is twice the size, at 1920 five times, and
    /// from 2720 wide the shift would go negative (undefined in the C) — while
    /// the clamps `d_pix_min = width/320`, `d_pix_max = width/80` do scale.
    /// Nearly every particle then sits at `d_pix_max`. The hires size keeps
    /// 320x200's at every scale: `pix = izi * xscale / (160 * 128)`, clamped
    /// to `[xscale/160, xscale/40 + 0.5]`, which is id's exactly at 320 and
    /// 640 wide (`xscale` 160 and 320 at fov 90) and follows the world's own
    /// projection at any other width, field of view or Hor+ widening.
    pub(crate) fn new(cam: &Camera, w: usize, h: usize, pixel_aspect: f32, hires: bool) -> Self {
        let (wf, hf) = (w as f32, h as f32);
        // horizontalFieldOfView = 2*tan(fov_x/2), a float; a degenerate fov
        // falls back to ~90 degrees as `Projection` does.
        let hfov = (2.0 * (cam.fov_deg as f64 * 0.5).to_radians().tan()) as f32;
        let hfov = if hfov.abs() < 2e-6 { 2.0 } else { hfov };
        let xscaleshrink = (w as i64 - 6) as f32 / hfov;
        let (pix_min, pix_max, pix_mul, pix_shift) = if hires {
            // izi * xscale / 20480 as 16.16: xscale*65536/20480 = xscale*3.2.
            let xscale = wf / hfov;
            let pix_mul = ((xscale as f64 * 3.2).round() as i64).max(1);
            let pix_min = ((xscale / 160.0) as i64).max(1);
            let pix_max = ((xscale / 40.0 + 0.5) as i64).max(1);
            (pix_min, pix_max, pix_mul, 16)
        } else {
            let pix_min = (w as i64 / 320).max(1);
            let pix_max = ((wf / 80.0 + 0.5) as i64).max(1);
            // d_pix_shift = 8 - (int)(width/320 + 0.5); negative (a view over
            // 2720 wide) would be undefined in the C.
            let pix_shift = (8 - (wf / 320.0 + 0.5) as i64).max(0) as u32;
            (pix_min, pix_max, 1, pix_shift)
        };
        let y_aspect_shift = u32::from(pixel_aspect > 1.4);
        ParticleProjection {
            xcenter: wf * 0.5 - 0.5,
            ycenter: hf * 0.5 - 0.5,
            xscaleshrink,
            yscaleshrink: xscaleshrink * pixel_aspect,
            pix_min,
            pix_max,
            pix_mul,
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
    use crate::render::{demo_room, Scene};
    use crate::render::fixtures::render_once;

    // -- Engine particles (draw_particles: projection + z-test) ---------------

    #[test]
    fn draw_particles_in_front_changes_a_pixel() {
        // A camera at the origin looking down +X; a particle 100 units straight
        // ahead must project near screen centre and paint its palette colour over
        // a z-buffer that holds nothing nearer.
        let w = 80usize;
        let h = 60usize;
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let bg = 9u8;
        let mut img = Image::new(w, h, bg);
        let mut zbuf = vec![i16::MIN; w * h];

        draw_particles(&mut img, &mut zbuf, &cam, &[([100.0, 0.0, 0.0], 42)], w, h, 1.0);

        // Some pixel changed to the particle colour, and the matching z-buffer
        // slot now holds the particle's 1/z: (int)(0x8000 / 100) = 327.
        let painted = img.pixels.iter().filter(|&&p| p == 42).count();
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
        let row_of = |aspect: f32| {
            let mut img = Image::new(w, h, 0);
            let mut zbuf = vec![i16::MIN; w * h];
            draw_particles(&mut img, &mut zbuf, &cam, &[([100.0, 0.0, 30.0], 42)], w, h, aspect);
            let i = img.pixels.iter().position(|&p| p == 42).expect("particle drawn");
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
        let mut img = Image::new(w, h, 0);
        let mut zbuf = vec![0i16; w * h]; // id's d_pzbuffer: 0 = nothing nearer
        draw_particles(&mut img, &mut zbuf, &cam, &[(p, 42)], w, h, aspect);
        let px: Vec<(usize, usize)> =
            (0..w * h).filter(|&i| img.pixels[i] == 42).map(|i| (i % w, i / w)).collect();
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
            &Camera { pos: [0.0; 3], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 }, 320, 200, 0.8333333, false);
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
        let centre = |parts: &[(Vec3, u8)]| {
            let mut img = Image::new(w, h, 0);
            let mut zbuf = vec![0i16; w * h]; // id's d_pzbuffer: 0 = nothing nearer
            draw_particles(&mut img, &mut zbuf, &cam, parts, w, h, 1.0);
            img.pixels[100 * w + 160]
        };
        assert_eq!((zbuf_izi(1.0 / 150.0), zbuf_izi(1.0 / 150.1)), (218, 218));
        assert_eq!(centre(&[([150.0, 0.0, 0.0], 1), ([150.1, 0.0, 0.0], 2)]), 2);
        assert_eq!(centre(&[([150.1, 0.0, 0.0], 1), ([150.0, 0.0, 0.0], 2)]), 2);
        assert_eq!(centre(&[([140.0, 0.0, 0.0], 1), ([150.0, 0.0, 0.0], 2)]), 1);
    }

    #[test]
    fn draw_particles_behind_wall_is_z_tested_out() {
        // Same view, but pre-fill the z-buffer with a NEARER 1/z (a wall at
        // depth 10, 0x8000/10) everywhere. A particle at depth 100 is behind it
        // and must NOT be drawn (D_DrawParticle draws only where pz <= izi).
        let w = 80usize;
        let h = 60usize;
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let bg = 9u8;
        let mut img = Image::new(w, h, bg);
        let mut zbuf = vec![3276i16; w * h]; // a wall closer than the particle

        draw_particles(&mut img, &mut zbuf, &cam, &[([100.0, 0.0, 0.0], 42)], w, h, 1.0);

        // Nothing painted: the wall occludes the particle.
        assert!(
            img.pixels.iter().all(|&p| p == bg),
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
        let mut img = Image::new(w, h, 9);
        let mut zbuf = vec![65i16; w * h]; // a wall FARTHER than the particle (depth 500)

        draw_particles(&mut img, &mut zbuf, &cam, &[([100.0, 0.0, 0.0], 7)], w, h, 1.0);

        let painted = img.pixels.iter().filter(|&&p| p == 7).count();
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
        let bg = 9u8;
        let mut img = Image::new(w, h, bg);
        let mut zbuf = vec![i16::MIN; w * h];

        // -X is behind a camera looking down +X.
        draw_particles(&mut img, &mut zbuf, &cam, &[([-100.0, 0.0, 0.0], 0)], w, h, 1.0);
        assert!(img.pixels.iter().all(|&p| p == bg), "a particle behind the camera draws nothing");
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
        let without = render_once(&Scene::new(&bsp, cam, 160, 120, &pal));
        // A particle ~80 units in front of the camera (well before the +256 wall).
        let with = render_once(&Scene { particles: &[([-120.0, 0.0, 0.0], 251)], ..Scene::new(&bsp, cam, 160, 120, &pal) });
        assert_ne!(without.pixels, with.pixels, "a visible particle must change the frame");
        assert!(
            with.pixels.contains(&251),
            "the particle's palette colour must appear in the frame"
        );
    }

    #[test]
    fn hires_particles_keep_320x200_proportions() {
        // id's izi >> d_pix_shift halves per 320 columns while the width only
        // adds 320: the hires size is id's at 320 and 640 wide and in
        // proportion everywhere else. At depth 100 (izi 327), fov 90:
        let cam = Camera { pos: [0.0; 3], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let size = |w: usize, hires: bool| {
            let p = ParticleProjection::new(&cam, w, w * 5 / 8, 1.0, hires);
            ((327 * p.pix_mul) >> p.pix_shift).clamp(p.pix_min, p.pix_max)
        };
        for w in [320, 640] {
            let (id, hi) = (ParticleProjection::new(&cam, w, 200, 1.0, false), ParticleProjection::new(&cam, w, 200, 1.0, true));
            assert_eq!((hi.pix_min, hi.pix_max), (id.pix_min, id.pix_max), "{w}");
            for izi in [0, 1, 100, 127, 128, 327, 1000, 4096] {
                assert_eq!((izi * hi.pix_mul) >> hi.pix_shift, izi >> id.pix_shift, "{w} wide, izi {izi}");
            }
        }
        // 320: 2; 640: 5; id's 1280 gives 327>>4 = 20 (clamped to 16), hires 10.
        assert_eq!([size(320, true), size(640, true), size(1280, false), size(1280, true)], [2, 5, 16, 10]);
        // 3840: id's shift would be -4 (clamped to 0 here), so 327 -> its max 48;
        // hires 30, and a far one (depth 1000, izi 32) 3 -> the minimum 12.
        assert_eq!((size(3840, false), size(3840, true)), (48, 30));
        let p = ParticleProjection::new(&cam, 3840, 2160, 1.0, true);
        assert_eq!(((32 * p.pix_mul) >> p.pix_shift).clamp(p.pix_min, p.pix_max), 12);
        // Hor+ at 16:9 (fov_x 106.26): the 1440x1080 screen's sizes.
        let wide = Camera { fov_deg: crate::render::FovMode::HorPlus.fov_x(90.0, 1920, 1080, 1.0), ..cam };
        let (a, b) = (ParticleProjection::new(&wide, 1920, 1080, 1.0, true), ParticleProjection::new(&cam, 1440, 1080, 1.0, true));
        assert_eq!((a.pix_min, a.pix_max, a.pix_mul, a.pix_shift), (b.pix_min, b.pix_max, b.pix_mul, b.pix_shift));
    }
}
