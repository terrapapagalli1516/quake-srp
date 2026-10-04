//! Sprite (SPR) entities.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/r_sprite.c` (`R_DrawSprite`, `R_RotateSprite`,
//! `R_SetupAndDrawSprite`, `R_ClipSpriteFace`, `R_GetSpriteframe`) and
//! `WinQuake/d_sprite.c` (`D_DrawSprite`, `D_SpriteCalculateGradients`,
//! `D_SpriteScanLeftEdge`, `D_SpriteScanRightEdge`, `D_SpriteDrawSpans`).
//!
//! A sprite is a flat poster: a frame `width x height` texels, one texel a
//! world unit, around the entity's origin. Its type says which plane it lies
//! in ([`sprite_axes`]): parallel to the view (`SPR_VP_PARALLEL`: id1's
//! explosions, bubbles and light globes), upright and turned to the eye or to
//! the view plane (`SPR_FACING_UPRIGHT`, `SPR_VP_PARALLEL_UPRIGHT`), fixed in
//! the world by the entity's angles (`SPR_ORIENTED`: Scourge of Armagon's
//! bullet holes, flat on the wall), or parallel to the view and turned by the
//! entity's roll (`SPR_VP_PARALLEL_ORIENTED`). id builds the poster's four
//! corners in world space, clips them to the view's four sides, projects
//! them, and fills the polygon with spans: texture coordinates exact every 8
//! pixels and stepped in between, each pixel z-tested against the 16-bit
//! z-buffer and written into it, texel 255 transparent.
//!
//! The setup — the axes, the clipped and projected polygon, the gradients and
//! the span list — is done once a frame ([`SpriteDraw::prepare`]); each band
//! of the view draws the spans in its rows ([`SpriteDraw::draw`]).
//! `D_SpriteDrawSpans` derives everything about a span from its own `u, v`,
//! so a band draws the same pixels whatever the split.
//!
//! The span routine is `d_sprite.c`'s portable C, what the C oracle (built
//! without id386) draws. id's x86 build drew `d_spr8.s` instead, whose steps
//! keep three more fractional bits, whose segment ends clamp at 2048 rather
//! than 8, and whose last segment divides through `reciprocal_table`: texel
//! choices a fraction of a texel apart, not modelled. Ported loops keep id's
//! shape.

use super::band::Band;
use super::edge::{frustum_planes, view_edges};
use super::polyse::c_ftoi;
use super::{Frame, Projection};
use crate::math::{ROLL, Vec3, add, angle_vectors, dot, inverse, normalize, scale, sub};
use crate::spr::{
    Frame as SprFrame, SPR_FACING_UPRIGHT, SPR_ORIENTED, SPR_VP_PARALLEL, SPR_VP_PARALLEL_ORIENTED,
    SPR_VP_PARALLEL_UPRIGHT, Sprite, SpriteFrame,
};

/// A sprite-model entity to draw (Quake's `mod_sprite` entities: the
/// `s_explod.spr` explosion, bubbles, Scourge of Armagon's bullet holes).
pub struct SpriteInstance<'a> {
    pub sprite: &'a Sprite,
    /// The entity's origin: the poster's centre (its frame's `origin` offsets
    /// the picture from it).
    pub origin: Vec3,
    /// The entity's `angles` (pitch, yaw, roll): an `SPR_ORIENTED` sprite lies
    /// in the plane they give, an `SPR_VP_PARALLEL_ORIENTED` one turns by
    /// their roll; the other types ignore them.
    pub angles: Vec3,
    /// Top-level frame index (an unknown one draws frame 0); a group frame
    /// animates by the scene's time.
    pub frame: usize,
    /// How many of the scene's alias models ([`Scene::models`](super::Scene),
    /// in order) come before this sprite on id's entity list
    /// (`cl_visedicts`): `R_DrawEntitiesOnList` draws the two kinds in one
    /// pass, and where a model and a sprite meet at the same 16-bit 1/z (gibs
    /// in an explosion, a few units apart) the later one wins. `usize::MAX`:
    /// after them all.
    pub models_before: usize,
}

/// `MAXWORKINGVERTS` (r_local.h): the clip buffers' size. id `Sys_Error`s at
/// it; a four-cornered poster clipped by four planes has at most eight.
const MAXWORKINGVERTS: usize = 16 + 4;

/// `NEAR_CLIP` (r_local.h): the least view-space depth a projected corner
/// takes.
const NEAR_CLIP: f32 = 0.01;

/// cos(1 degree): an upright sprite seen within a degree of straight up or
/// down is not drawn, its cross product being of two nearly parallel vectors
/// (`R_DrawSprite`).
const COS_ONE_DEGREE: f64 = 0.999848;

/// A sprite's axes, `r_spritedesc.vpn`, `vright`, `vup`: the poster's normal
/// (pointing away from the side it is seen from), its rightward and its
/// upward direction, in world space.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Axes {
    vpn: Vec3,
    vright: Vec3,
    vup: Vec3,
}

/// What `R_ViewChanged` and `R_SetupFrame` leave for the sprites: the view's
/// axes and eye, the projection (`xcenter`, `xscale`, ..., id's pixel centres
/// on the integers), the view's rectangle (`r_refdef.fvrect*_adj`) and its
/// four sides in world space (`view_clipplanes`, the world's own: the edge
/// renderer's [`frustum_planes`]).
pub(super) struct SpriteView {
    view: Axes,
    origin: Vec3,
    xcenter: f32,
    ycenter: f32,
    xscale: f32,
    yscale: f32,
    xscaleinv: f32,
    yscaleinv: f32,
    fvrectx_adj: f32,
    fvrecty_adj: f32,
    fvrectright_adj: f32,
    fvrectbottom_adj: f32,
    clip: [(Vec3, f32); 4],
}

impl SpriteView {
    /// The sprites' view of `frame`, as the edge renderer sets it up
    /// (`EdgeState::setup_frame`).
    pub(super) fn new(frame: &Frame) -> SpriteView {
        let (cam, w, h) = (&frame.cam, frame.w, frame.h);
        let proj = Projection::new(cam, &frame.geom, frame.scene.options.aspect());
        let Projection { cx, cy, xscale, yscale } = proj;
        let (vpn, vright, vup) = cam.basis();
        let (wf, hf) = (w as f32, h as f32);
        SpriteView {
            view: Axes { vpn, vright, vup },
            origin: cam.pos,
            xcenter: cx - 0.5,
            ycenter: cy - 0.5,
            xscale,
            yscale,
            xscaleinv: 1.0 / xscale,
            yscaleinv: 1.0 / yscale,
            fvrectx_adj: -0.5,
            fvrecty_adj: -0.5,
            fvrectright_adj: wf - 0.5,
            fvrectbottom_adj: hf - 0.5,
            clip: frustum_planes(&view_edges(&frame.geom, &proj), vpn, vright, vup, cam.pos),
        }
    }

    /// `TransformVector`: `v` along the view's right, up and forward axes.
    fn transform(&self, v: Vec3) -> Vec3 {
        [dot(v, self.view.vright), dot(v, self.view.vup), dot(v, self.view.vpn)]
    }
}

/// `R_DrawSprite`'s axes for a sprite of type `kind` seen through a view whose
/// axes are `view`, from `modelorg` (the eye less the entity's origin), the
/// entity turned by `angles`. `None` where id draws nothing: an upright type
/// seen within a degree of straight up or down, and a type id does not know
/// (id's `Sys_Error`).
fn sprite_axes(kind: i32, view: &Axes, modelorg: Vec3, angles: Vec3) -> Option<Axes> {
    // `vup` straight up, `vright` level and perpendicular to `dir`, `vpn`
    // level and perpendicular to both. (id's comments name the cross
    // products the other way round; the components are these.)
    let upright = |dir: Vec3| {
        // dot(dir, vup) with vup (0, 0, 1); compared in double, as the C.
        if f64::from(dir[2]).abs() > COS_ONE_DEGREE {
            return None;
        }
        // dir x vup, normalized.
        let (vright, _) = normalize([dir[1], -dir[0], 0.0]);
        // vup x vright.
        Some(Axes { vpn: [-vright[1], vright[0], 0.0], vright, vup: [0.0, 0.0, 1.0] })
    };
    match kind {
        // Turned to the eye: the direction to it is -modelorg.
        SPR_FACING_UPRIGHT => upright(normalize(inverse(modelorg)).0),
        // Parallel to the view plane: the view's own axes.
        SPR_VP_PARALLEL => Some(*view),
        // Parallel to the view plane, but upright.
        SPR_VP_PARALLEL_UPRIGHT => upright(view.vpn),
        // As the entity's angles turn it (AngleVectors).
        SPR_ORIENTED => {
            let (vpn, vright, vup) = angle_vectors(angles);
            Some(Axes { vpn, vright, vup })
        }
        // Parallel to the view plane, rotated in it by the entity's roll:
        // `vpn` stays, `vright` and `vup` turn.
        SPR_VP_PARALLEL_ORIENTED => {
            // `float angle = angles[ROLL] * (M_PI*2 / 360)`; sin and cos in
            // double, kept as floats.
            let angle = (f64::from(angles[ROLL]) * (std::f64::consts::PI * 2.0 / 360.0)) as f32;
            let (sr, cr) = (f64::from(angle).sin() as f32, f64::from(angle).cos() as f32);
            let (r, u) = (view.vright, view.vup);
            Some(Axes {
                vpn: view.vpn,
                vright: [r[0] * cr + u[0] * sr, r[1] * cr + u[1] * sr, r[2] * cr + u[2] * sr],
                vup: [r[0] * -sr + u[0] * cr, r[1] * -sr + u[1] * cr, r[2] * -sr + u[2] * cr],
            })
        }
        _ => None,
    }
}

/// `R_GetSpriteframe`: the picture `frame` shows at `time` (`cl.time`; the
/// port gives every sprite a syncbase of 0). An index the sprite does not
/// have shows frame 0 (id prints "no such frame"); a group shows the first
/// sub-frame whose interval ends after `time` modulo the group's length.
fn sprite_frame(sprite: &Sprite, frame: usize, time: f32) -> Option<&SpriteFrame> {
    match sprite.frames.get(frame).or_else(|| sprite.frames.first())? {
        SprFrame::Single(f) => Some(f),
        SprFrame::Group { intervals, frames } => {
            let numframes = frames.len().min(intervals.len());
            let fullinterval = *intervals.get(numframes.checked_sub(1)?)?;
            // `time - ((int)(time / fullinterval)) * fullinterval`; the
            // loader keeps every interval above 0.
            let targettime = time - (time / fullinterval) as i32 as f32 * fullinterval;
            let i = intervals[..numframes - 1].iter().position(|&iv| iv > targettime).unwrap_or(numframes - 1);
            frames.get(i)
        }
    }
}

/// `R_ClipSpriteFace`: the part of the winding `input` on the inner side of
/// `plane` (`dot(normal, p) - dist >= 0`), into `out`. id carries each
/// point's texel `s, t` along (`vec5_t`), which nothing downstream reads; the
/// points alone are clipped here.
fn clip_sprite_face(input: &[Vec3], plane: &(Vec3, f32), out: &mut Vec<Vec3>) {
    out.clear();
    let (normal, clipdist) = *plane;
    let dists: Vec<f32> = input.iter().map(|&p| dot(p, normal) - clipdist).collect();
    let n = input.len();
    for i in 0..n {
        // The winding wraps: the point after the last is the first.
        let (p, next) = (input[i], input[(i + 1) % n]);
        let (d, dnext) = (dists[i], dists[(i + 1) % n]);
        if d >= 0.0 {
            out.push(p);
        }
        if d == 0.0 || dnext == 0.0 || (d > 0.0) == (dnext > 0.0) {
            continue;
        }
        // Split the edge where it crosses the plane.
        let frac = d / (d - dnext);
        out.push([p[0] + frac * (next[0] - p[0]), p[1] + frac * (next[1] - p[1]), p[2] + frac * (next[2] - p[2])]);
    }
}

/// `emitpoint_t`: a corner of the clipped poster on the screen, id's pixel
/// centres on the integers. (id keeps its `s, t` and `1/z` too; the span
/// setup reads neither.)
#[derive(Clone, Copy, Debug)]
struct EmitPoint {
    u: f32,
    v: f32,
}

/// `sspan_t`: `count` pixels of row `v` from column `u`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SSpan {
    u: i32,
    v: i32,
    count: i32,
}

/// What `D_SpriteCalculateGradients` leaves for `D_SpriteDrawSpans`: `s/z`,
/// `t/z` and `1/z` as linear functions of the screen's `(u, v)`, and the
/// 16.16 offsets and limits that put `s, t` on the frame's texels.
#[derive(Clone, Copy, Debug)]
struct Gradients {
    sdivzstepu: f32,
    tdivzstepu: f32,
    sdivzstepv: f32,
    tdivzstepv: f32,
    zistepu: f32,
    zistepv: f32,
    sdivzorigin: f32,
    tdivzorigin: f32,
    ziorigin: f32,
    sadjust: i32,
    tadjust: i32,
    bbextents: i32,
    bbextentt: i32,
}

impl Gradients {
    /// `D_SpriteCalculateGradients` for a poster on `axes` seen from
    /// `modelorg`, its frame `cachewidth x height` texels.
    fn new(view: &SpriteView, axes: &Axes, modelorg: Vec3, cachewidth: i32, height: i32) -> Gradients {
        let p_normal = view.transform(axes.vpn);
        let p_saxis = view.transform(axes.vright);
        let p_taxis = inverse(view.transform(axes.vup));
        let distinv = 1.0 / (-dot(modelorg, axes.vpn));
        let sdivzstepu = p_saxis[0] * view.xscaleinv;
        let tdivzstepu = p_taxis[0] * view.xscaleinv;
        let sdivzstepv = -p_saxis[1] * view.yscaleinv;
        let tdivzstepv = -p_taxis[1] * view.yscaleinv;
        let zistepu = p_normal[0] * view.xscaleinv * distinv;
        let zistepv = -p_normal[1] * view.yscaleinv * distinv;
        let p_temp1 = view.transform(modelorg);
        // `(fixed16_t)(DotProduct (p_temp1, axis) * 0x10000 + 0.5)` (the 0.5
        // a double), less `-(size >> 1) << 16`: texel 0 is half the frame
        // left of (above) the entity's origin.
        let adjust = |axis: Vec3, size: i32| {
            c_ftoi(f64::from(dot(p_temp1, axis) * 65536.0) + 0.5).wrapping_sub(-(size >> 1) << 16)
        };
        Gradients {
            sdivzstepu,
            tdivzstepu,
            sdivzstepv,
            tdivzstepv,
            zistepu,
            zistepv,
            sdivzorigin: p_saxis[2] - view.xcenter * sdivzstepu - view.ycenter * sdivzstepv,
            tdivzorigin: p_taxis[2] - view.xcenter * tdivzstepu - view.ycenter * tdivzstepv,
            ziorigin: p_normal[2] * distinv - view.xcenter * zistepu - view.ycenter * zistepv,
            sadjust: adjust(p_saxis, cachewidth),
            tadjust: adjust(p_taxis, height),
            // -1 (-epsilon) so we never wander off the edge of the texture
            bbextents: (cachewidth << 16).wrapping_sub(1),
            bbextentt: (height << 16).wrapping_sub(1),
        }
    }
}

/// One sprite of the frame, ready for the bands: what `D_DrawSprite` hands
/// `D_SpriteDrawSpans`.
pub(super) struct SpriteDraw<'a> {
    /// `cacheblock`: the frame's texels, `cachewidth` a row.
    pixels: &'a [u8],
    cachewidth: i32,
    grads: Gradients,
    /// `sprite_spans`: one span a row, top down, as the edge walkers leave
    /// them.
    spans: Vec<SSpan>,
}

impl<'a> SpriteDraw<'a> {
    /// `R_DrawEntitiesOnList`'s sprite case up to the spans: `R_DrawSprite`
    /// (the frame, the axes, `R_RotateSprite`), `R_SetupAndDrawSprite` (the
    /// poster built, back-face culled, clipped to the view and projected) and
    /// `D_DrawSprite`'s setup (the gradients and the span list). `None` for a
    /// sprite that draws nothing.
    pub(super) fn prepare(view: &SpriteView, inst: &SpriteInstance<'a>, time: f32) -> Option<SpriteDraw<'a>> {
        let sprite = inst.sprite;
        let frame = sprite_frame(sprite, inst.frame, time)?;
        let (width, height) = (frame.width, frame.height);
        // Not in id's data (a frame of no texels has nothing to draw).
        if width <= 0 || height <= 0 {
            return None;
        }
        // R_DrawEntitiesOnList: r_entorigin and modelorg.
        let mut entorigin = inst.origin;
        let mut modelorg = sub(view.origin, entorigin);
        let axes = sprite_axes(sprite.header.type_, &view.view, modelorg, inst.angles)?;
        // R_RotateSprite: a beam sprite's origin moves back along its normal.
        let beamlength = sprite.header.beamlength;
        if beamlength != 0.0 {
            let vec = scale(axes.vpn, -beamlength);
            entorigin = add(entorigin, vec);
            modelorg = sub(modelorg, vec);
        }
        let pverts = setup_sprite(view, &axes, frame, entorigin, modelorg)?;
        let spans = scan_spans(view, &pverts)?;
        Some(SpriteDraw {
            pixels: &frame.pixels,
            cachewidth: width,
            grads: Gradients::new(view, &axes, modelorg, width, height),
            spans,
        })
    }

    /// `D_SpriteDrawSpans` over the spans in `band`'s rows.
    pub(super) fn draw(&self, band: &mut Band) {
        let Some(first) = self.spans.first() else { return };
        // The spans are one a row from `first.v` down.
        let rows = band.rows();
        let skip = (rows.start as i64 - i64::from(first.v)).max(0) as usize;
        let take = (rows.end as i64 - i64::from(first.v)).max(0) as usize;
        for sp in self.spans.iter().take(take).skip(skip) {
            self.draw_span(band, *sp);
        }
    }

    /// `D_SpriteDrawSpans`' loop body for one span: `s/z`, `t/z`, `1/z` exact
    /// at its first pixel and every 8 pixels (the last segment's end at its
    /// last pixel), `s, t` stepped in 16.16 in between and clamped to the
    /// frame. A pixel is drawn where its texel is not 255 and its 1/z is not
    /// behind the z-buffer's (`*pz <= izi >> 16`), and writes its 1/z.
    fn draw_span(&self, band: &mut Band, sp: SSpan) {
        let g = &self.grads;
        let mut count = sp.count;
        if count <= 0 {
            return;
        }
        let w = band.width();
        let Some((prow, zrow)) = usize::try_from(sp.v).ok().and_then(|v| band.span(0, v, w)) else { return };
        let (sdivz8stepu, tdivz8stepu, zi8stepu) = (g.sdivzstepu * 8.0, g.tdivzstepu * 8.0, g.zistepu * 8.0);
        // We count on FP exceptions being turned off to avoid range problems.
        let izistep = c_ftoi(f64::from(g.zistepu * 32768.0 * 65536.0));
        let (du, dv) = (sp.u as f32, sp.v as f32);
        let mut sdivz = g.sdivzorigin + dv * g.sdivzstepv + du * g.sdivzstepu;
        let mut tdivz = g.tdivzorigin + dv * g.tdivzstepv + du * g.tdivzstepu;
        let mut zi = g.ziorigin + dv * g.zistepv + du * g.zistepu;
        // Prescale to 16.16 fixed-point.
        let mut z = 65536.0 / zi;
        let mut izi = c_ftoi(f64::from(zi * 32768.0 * 65536.0));
        let clamp = |x: i32, lo: i32, hi: i32| {
            if x > hi {
                hi
            } else if x < lo {
                lo
            } else {
                x
            }
        };
        let fix = |divz: f32, z: f32, adjust: i32| c_ftoi(f64::from(divz * z)).wrapping_add(adjust);
        let mut s = clamp(fix(sdivz, z, g.sadjust), 0, g.bbextents);
        let mut t = clamp(fix(tdivz, z, g.tadjust), 0, g.bbextentt);
        let (mut sstep, mut tstep) = (0i32, 0i32);
        let mut x = i64::from(sp.u);
        loop {
            // s and t at the far end of this segment.
            let spancount = count.min(8);
            count -= spancount;
            if count > 0 {
                // A full segment: steps by shifting.
                sdivz += sdivz8stepu;
                tdivz += tdivz8stepu;
                zi += zi8stepu;
                z = 65536.0 / zi;
            } else {
                // The last: land on its last pixel, so as not to step off the
                // polygon; steps by division, biased low.
                let spancountminus1 = (spancount - 1) as f32;
                sdivz += g.sdivzstepu * spancountminus1;
                tdivz += g.tdivzstepu * spancountminus1;
                zi += g.zistepu * spancountminus1;
                z = 65536.0 / zi;
            }
            // The low clamp keeps round-off on a negative step from
            // overstepping the frame's edge.
            let snext = clamp(fix(sdivz, z, g.sadjust), 8, g.bbextents);
            let tnext = clamp(fix(tdivz, z, g.tadjust), 8, g.bbextentt);
            if count > 0 {
                sstep = (snext - s) >> 3;
                tstep = (tnext - t) >> 3;
            } else if spancount > 1 {
                sstep = (snext - s) / (spancount - 1);
                tstep = (tnext - t) / (spancount - 1);
            }
            for _ in 0..spancount {
                let texel = usize::try_from(i64::from(s >> 16) + i64::from(t >> 16) * i64::from(self.cachewidth))
                    .ok()
                    .and_then(|i| self.pixels.get(i).copied());
                // id's spans lie inside the view; the check only keeps a
                // stray one (no id frame has one) off the neighbouring rows.
                let px = usize::try_from(x).ok().filter(|&px| px < w);
                if let (Some(texel), Some(px)) = (texel, px) {
                    if texel != 255 && i32::from(zrow[px]) <= izi >> 16 {
                        zrow[px] = (izi >> 16) as i16;
                        prow[px] = texel;
                    }
                }
                izi = izi.wrapping_add(izistep);
                x += 1;
                s = s.wrapping_add(sstep);
                t = t.wrapping_add(tstep);
            }
            s = snext;
            t = tnext;
            if count <= 0 {
                break;
            }
        }
    }
}

/// `R_SetupAndDrawSprite` up to `D_DrawSprite`: the poster on `axes` around
/// `entorigin`, culled when its back faces the eye, clipped to the view's
/// four sides in world space, and projected (each corner at least
/// `NEAR_CLIP` deep). The corners come back in order with the first repeated
/// at the end (`D_DrawSprite`'s copy, so the edge walkers need not wrap);
/// `None` when nothing is left.
fn setup_sprite(
    view: &SpriteView,
    axes: &Axes,
    frame: &SpriteFrame,
    entorigin: Vec3,
    modelorg: Vec3,
) -> Option<Vec<EmitPoint>> {
    // Backface cull.
    if dot(axes.vpn, modelorg) >= 0.0 {
        return None;
    }
    // Mod_LoadSpriteFrame's edges of the picture about the origin.
    let up_ofs = frame.origin[1] as f32;
    let down_ofs = frame.origin[1].wrapping_sub(frame.height) as f32;
    let left_ofs = frame.origin[0] as f32;
    let right_ofs = frame.width.wrapping_add(frame.origin[0]) as f32;
    // The poster in world space, clockwise from the top left as seen.
    let (right, up) = (scale(axes.vright, right_ofs), scale(axes.vup, up_ofs));
    let (left, down) = (scale(axes.vright, left_ofs), scale(axes.vup, down_ofs));
    let corner = |a: Vec3, b: Vec3| add(add(entorigin, a), b);
    let mut winding = vec![corner(up, left), corner(up, right), corner(down, right), corner(down, left)];
    let mut clipped = Vec::with_capacity(MAXWORKINGVERTS);
    for plane in &view.clip {
        clip_sprite_face(&winding, plane, &mut clipped);
        std::mem::swap(&mut winding, &mut clipped);
        // id Sys_Errors at MAXWORKINGVERTS, which clipping a 4-gon by four
        // planes never reaches.
        if winding.len() < 3 || winding.len() >= MAXWORKINGVERTS {
            return None;
        }
    }
    // Into view space, and projected.
    let mut pverts: Vec<EmitPoint> = winding
        .iter()
        .map(|&p| {
            let mut transformed = view.transform(sub(p, view.origin));
            if transformed[2] < NEAR_CLIP {
                transformed[2] = NEAR_CLIP;
            }
            let zi = 1.0 / transformed[2];
            EmitPoint {
                u: view.xcenter + view.xscale * zi * transformed[0],
                v: view.ycenter - view.yscale * zi * transformed[1],
            }
        })
        .collect();
    pverts.push(pverts[0]);
    Some(pverts)
}

/// `D_DrawSprite`'s span list for the projected polygon `pverts` (its first
/// corner repeated at the end): `D_SpriteScanLeftEdge` walks from the top
/// corner backwards down the left side, setting each row's `u` (ceil'd, as
/// the rows are the `ceil`s of the corners' `v`), and `D_SpriteScanRightEdge`
/// forwards down the right side, clamped to the view, setting each row's
/// `count`. `None` when the polygon crosses no row, or (never for a polygon
/// clipped to the view) strays more than a pixel outside it.
fn scan_spans(view: &SpriteView, pverts: &[EmitPoint]) -> Option<Vec<SSpan>> {
    let nump = pverts.len().checked_sub(1)?;
    let inside = |p: &EmitPoint| {
        p.u >= view.fvrectx_adj - 1.0
            && p.u <= view.fvrectright_adj + 1.0
            && p.v >= view.fvrecty_adj - 1.0
            && p.v <= view.fvrectbottom_adj + 1.0
    };
    if nump < 3 || !pverts.iter().all(inside) {
        return None;
    }
    // The top and bottom corners (the first of equals).
    let (mut ymin, mut ymax) = (999_999.9f32, -999_999.9f32);
    let (mut minindex, mut maxindex) = (0, 0);
    for (i, p) in pverts[..nump].iter().enumerate() {
        if p.v < ymin {
            ymin = p.v;
            minindex = i;
        }
        if p.v > ymax {
            ymax = p.v;
            maxindex = i;
        }
    }
    if ymin.ceil() >= ymax.ceil() {
        return None; // doesn't cross any scans at all
    }

    // One edge's rows from `vtop` to before `vbottom`: the first row's `u`
    // (16.16, ceil'd by adding just under one) and the step a row.
    let edge = |uvert: f32, vvert: f32, unext: f32, vnext: f32, vtop: f32| {
        let slope = (unext - uvert) / (vnext - vvert);
        let u_step = c_ftoi(f64::from(slope * 65536.0));
        let u = c_ftoi(f64::from((uvert + slope * (vtop - vvert)) * 65536.0)).wrapping_add(0x10000 - 1);
        (u, u_step)
    };

    // D_SpriteScanLeftEdge
    let mut spans: Vec<SSpan> = Vec::new();
    let mut i = if minindex == 0 { nump } else { minindex };
    let lmaxindex = if maxindex == 0 { nump } else { maxindex };
    let mut vtop = pverts[i].v.ceil();
    loop {
        let (pvert, pnext) = (pverts[i], pverts[i - 1]);
        let vbottom = pnext.v.ceil();
        if vtop < vbottom {
            let (mut u, u_step) = edge(pvert.u, pvert.v, pnext.u, pnext.v, vtop);
            for v in vtop as i32..vbottom as i32 {
                spans.push(SSpan { u: u >> 16, v, count: 0 });
                u = u.wrapping_add(u_step);
            }
        }
        vtop = vbottom;
        i -= 1;
        if i == 0 {
            i = nump;
        }
        if i == lmaxindex {
            break;
        }
    }

    // D_SpriteScanRightEdge
    let clamp_v = |v: f32| {
        if v < view.fvrecty_adj {
            view.fvrecty_adj
        } else if v > view.fvrectbottom_adj {
            view.fvrectbottom_adj
        } else {
            v
        }
    };
    let clamp_u = |u: f32| {
        if u < view.fvrectx_adj {
            view.fvrectx_adj
        } else if u > view.fvrectright_adj {
            view.fvrectright_adj
        } else {
            u
        }
    };
    let mut k = 0;
    let mut i = minindex;
    let mut vvert = clamp_v(pverts[i].v);
    let mut vtop = vvert.ceil();
    loop {
        let (pvert, pnext) = (pverts[i], pverts[i + 1]);
        let vnext = clamp_v(pnext.v);
        let vbottom = vnext.ceil();
        if vtop < vbottom {
            let (mut u, u_step) = edge(clamp_u(pvert.u), vvert, clamp_u(pnext.u), vnext, vtop);
            for _ in vtop as i32..vbottom as i32 {
                // id writes on past the left edge's rows into a stale list;
                // nothing clipped to the view does.
                if let Some(sp) = spans.get_mut(k) {
                    sp.count = (u >> 16) - sp.u;
                }
                u = u.wrapping_add(u_step);
                k += 1;
            }
        }
        vtop = vbottom;
        vvert = vnext;
        i += 1;
        if i == nump {
            i = 0;
        }
        if i == maxindex {
            break;
        }
    }
    // The end of the span list (DS_SPAN_LIST_END).
    spans.truncate(k);
    Some(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::{cross, length};
    use crate::render::fixtures::test_sprite;
    use crate::render::{Camera, Image, Scene};

    /// Every sprite of `sprites` seen by `cam` at `time`, into `img` and
    /// `zbuf`, as the renderer's bands draw them.
    fn draw(img: &mut Image, zbuf: &mut [i16], cam: Camera, sprites: &[SpriteInstance], time: f32) {
        let world = crate::render::demo_room();
        let scene = Scene { sprites, time, ..Scene::new(&world, cam, img.w, img.h, &[[0; 3]; 256]) };
        let frame = Frame::new(&scene, img.w, img.h);
        let view = SpriteView::new(&frame);
        let mut band = Band::whole(img.w, &mut img.pixels, zbuf);
        for inst in sprites {
            if let Some(d) = SpriteDraw::prepare(&view, inst, time) {
                d.draw(&mut band);
            }
        }
    }

    fn cam_at_origin() -> Camera {
        Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 }
    }

    fn sprite_of_type(kind: i32, w: i32, h: i32, fill: u8) -> Sprite {
        let mut spr = test_sprite(w, h, fill);
        spr.header.type_ = kind;
        spr
    }

    fn close(a: Vec3, b: Vec3) -> bool {
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-6)
    }

    /// The view `(vpn, vright, vup)` of a camera at `pitch` (up positive) and
    /// `yaw`, as the renderer builds it.
    fn view_axes(pitch: f32, yaw: f32) -> Axes {
        let cam = Camera { pitch, yaw, ..cam_at_origin() };
        let (vpn, vright, vup) = cam.basis();
        Axes { vpn, vright, vup }
    }

    #[test]
    fn vp_parallel_takes_the_views_axes() {
        // R_DrawSprite, SPR_VP_PARALLEL: the view's own vpn/vright/vup,
        // wherever the sprite is.
        let view = view_axes(20.0, 35.0);
        for modelorg in [[-100.0, 3.0, 7.0], [0.0, 0.0, -50.0]] {
            assert_eq!(sprite_axes(SPR_VP_PARALLEL, &view, modelorg, [0.0, 0.0, 33.0]), Some(view));
        }
    }

    #[test]
    fn facing_upright_turns_to_the_eye_and_stands_up() {
        // SPR_FACING_UPRIGHT: vup straight up; vright level and perpendicular
        // to the eye's direction; vpn level, pointing away from the eye.
        let view = view_axes(0.0, 0.0);
        // The eye 100 units west and 30 below the sprite: modelorg = eye - origin.
        let modelorg = [-100.0, 0.0, -30.0];
        let a = sprite_axes(SPR_FACING_UPRIGHT, &view, modelorg, [0.0; 3]).unwrap();
        assert_eq!(a.vup, [0.0, 0.0, 1.0]);
        assert!(close(a.vright, [0.0, -1.0, 0.0]), "{:?}", a.vright);
        assert!(close(a.vpn, [1.0, 0.0, 0.0]), "{:?}", a.vpn);
        assert!(dot(a.vpn, modelorg) < 0.0, "seen from its front");
        // The eye off to the side: vright stays perpendicular to it, level.
        let modelorg = [-60.0, -80.0, 10.0];
        let a = sprite_axes(SPR_FACING_UPRIGHT, &view, modelorg, [0.0; 3]).unwrap();
        assert!(dot(a.vright, [modelorg[0], modelorg[1], 0.0]).abs() < 1e-4);
        assert_eq!((a.vright[2], a.vpn[2]), (0.0, 0.0));
        assert!((length(a.vright) - 1.0).abs() < 1e-6);
        assert!(close(a.vpn, cross(a.vup, a.vright)), "vpn = vup x vright");
        // Straight above or below it (within a degree): not drawn.
        assert_eq!(sprite_axes(SPR_FACING_UPRIGHT, &view, [0.5, 0.0, 100.0], [0.0; 3]), None);
        assert_eq!(sprite_axes(SPR_FACING_UPRIGHT, &view, [0.0, 0.5, -100.0], [0.0; 3]), None);
        assert!(sprite_axes(SPR_FACING_UPRIGHT, &view, [2.0, 0.0, 100.0], [0.0; 3]).is_some(), "1.15 degrees off");
    }

    #[test]
    fn vp_parallel_upright_follows_the_view_plane_and_stands_up() {
        // SPR_VP_PARALLEL_UPRIGHT: vup straight up; vright the view's
        // vright made level; wherever the sprite is.
        let view = view_axes(-30.0, 60.0);
        let a = sprite_axes(SPR_VP_PARALLEL_UPRIGHT, &view, [5.0, -9.0, 40.0], [0.0; 3]).unwrap();
        assert_eq!(a.vup, [0.0, 0.0, 1.0]);
        assert!(close(a.vright, view.vright), "{:?} {:?}", a.vright, view.vright);
        let (level_vpn, _) = normalize([view.vpn[0], view.vpn[1], 0.0]);
        assert!(close(a.vpn, level_vpn), "{:?}", a.vpn);
        // Looking (nearly) straight down: not drawn.
        let down = view_axes(-89.5, 60.0);
        assert_eq!(sprite_axes(SPR_VP_PARALLEL_UPRIGHT, &down, [5.0, -9.0, 40.0], [0.0; 3]), None);
    }

    #[test]
    fn oriented_lies_in_the_plane_of_its_angles() {
        // SPR_ORIENTED: AngleVectors of the entity's angles, whatever the
        // view. A bullet hole on a wall facing -Y (Scourge's
        // placebullethole: angles = vectoangles of the wall's normal less
        // 180) has vpn +Y, into the wall, and lies in the wall's XZ plane.
        let view = view_axes(10.0, 165.0);
        let a = sprite_axes(SPR_ORIENTED, &view, [77.0, -16.75, 2.875], [0.0, 90.0, 0.0]).unwrap();
        assert!(close(a.vpn, [0.0, 1.0, 0.0]), "{:?}", a.vpn);
        assert!(close(a.vright, [1.0, 0.0, 0.0]), "{:?}", a.vright);
        assert!(close(a.vup, [0.0, 0.0, 1.0]), "{:?}", a.vup);
        // Any angles: the AngleVectors basis itself.
        let angles = [30.0, -45.0, 12.0];
        let (vpn, vright, vup) = angle_vectors(angles);
        assert_eq!(sprite_axes(SPR_ORIENTED, &view, [1.0; 3], angles), Some(Axes { vpn, vright, vup }));
    }

    #[test]
    fn vp_parallel_oriented_rolls_in_the_view_plane() {
        // SPR_VP_PARALLEL_ORIENTED: vpn the view's; vright and vup turned by
        // the entity's roll in the view plane.
        let view = view_axes(15.0, 40.0);
        let a = sprite_axes(SPR_VP_PARALLEL_ORIENTED, &view, [-9.0; 3], [0.0, 0.0, 90.0]).unwrap();
        assert_eq!(a.vpn, view.vpn);
        assert!(close(a.vright, view.vup), "a quarter turn: vright onto vup");
        assert!(close(a.vup, inverse(view.vright)));
        let a = sprite_axes(SPR_VP_PARALLEL_ORIENTED, &view, [-9.0; 3], [0.0, 0.0, 0.0]).unwrap();
        assert!(close(a.vright, view.vright) && close(a.vup, view.vup));
    }

    #[test]
    fn an_unknown_type_draws_nothing() {
        // id's Sys_Error ("Bad sprite type"); the port skips the sprite.
        let view = view_axes(0.0, 0.0);
        assert_eq!(sprite_axes(5, &view, [-100.0, 0.0, 0.0], [0.0; 3]), None);
        assert_eq!(sprite_axes(-1, &view, [-100.0, 0.0, 0.0], [0.0; 3]), None);
    }

    #[test]
    fn a_vp_parallel_sprite_ahead_fills_its_square_at_its_depth() {
        // 16x16 texels at 100 units, fov 90 on an 80-wide view: xscale 40,
        // 6.4 pixels across, centred on the view.
        let (w, h) = (80usize, 60usize);
        let mut img = Image::new(w, h, 9);
        let mut zbuf = vec![i16::MIN; w * h];
        let spr = sprite_of_type(SPR_VP_PARALLEL, 16, 16, 42);
        let inst =
            SpriteInstance { sprite: &spr, origin: [100.0, 0.0, 0.0], angles: [0.0; 3], frame: 0, models_before: 0 };
        draw(&mut img, &mut zbuf, cam_at_origin(), std::slice::from_ref(&inst), 0.0);
        let painted: Vec<(usize, usize)> =
            (0..w * h).filter(|&i| img.pixels[i] == 42).map(|i| (i % w, i / w)).collect();
        // u from 36.3 to 42.7 -> columns 37..=42; rows likewise 27..=32.
        let (xs, ys): (Vec<_>, Vec<_>) = painted.iter().copied().unzip();
        assert_eq!((xs.iter().min(), xs.iter().max()), (Some(&37), Some(&42)));
        assert_eq!((ys.iter().min(), ys.iter().max()), (Some(&27), Some(&32)));
        assert_eq!(painted.len(), 36);
        // (int)(1/100 * 0x8000 * 0x10000) >> 16 = 327, at every pixel.
        assert!(painted.iter().all(|&(x, y)| zbuf[y * w + x] == 327));
        assert_eq!(zbuf.iter().filter(|&&z| z != i16::MIN).count(), 36);
    }

    #[test]
    fn index_255_is_transparent() {
        // Texel 255 neither paints nor writes 1/z (d_sprite.c).
        let (w, h) = (80usize, 60usize);
        let mut img = Image::new(w, h, 9);
        let mut zbuf = vec![i16::MIN; w * h];
        let spr = sprite_of_type(SPR_VP_PARALLEL, 16, 16, 255);
        let inst =
            SpriteInstance { sprite: &spr, origin: [100.0, 0.0, 0.0], angles: [0.0; 3], frame: 0, models_before: 0 };
        draw(&mut img, &mut zbuf, cam_at_origin(), std::slice::from_ref(&inst), 0.0);
        assert!(img.pixels.iter().all(|&p| p == 9));
        assert!(zbuf.iter().all(|&z| z == i16::MIN));
    }

    #[test]
    fn a_sprite_behind_a_nearer_wall_is_hidden() {
        // The z-buffer holds a wall at depth 10 (1/z 3276): the sprite at 100 fails.
        let (w, h) = (80usize, 60usize);
        let mut img = Image::new(w, h, 9);
        let mut zbuf = vec![3276i16; w * h];
        let spr = sprite_of_type(SPR_VP_PARALLEL, 16, 16, 42);
        let inst =
            SpriteInstance { sprite: &spr, origin: [100.0, 0.0, 0.0], angles: [0.0; 3], frame: 0, models_before: 0 };
        draw(&mut img, &mut zbuf, cam_at_origin(), std::slice::from_ref(&inst), 0.0);
        assert!(img.pixels.iter().all(|&p| p == 9));
    }

    #[test]
    fn an_oriented_sprite_seen_obliquely_is_a_perspective_trapezoid() {
        // A 32x32 poster turned 45 degrees (yaw), 100 units ahead: its
        // screen-left edge is 89 deep, its right 111, so the left is taller
        // on screen (28 rows against 24) and its 1/z falls to the right.
        let (w, h) = (160usize, 120usize);
        let mut img = Image::new(w, h, 9);
        let mut zbuf = vec![i16::MIN; w * h];
        let spr = sprite_of_type(SPR_ORIENTED, 32, 32, 42);
        let inst = SpriteInstance {
            sprite: &spr,
            origin: [100.0, 0.0, 0.0],
            angles: [0.0, 45.0, 0.0],
            frame: 0,
            models_before: 0,
        };
        draw(&mut img, &mut zbuf, cam_at_origin(), std::slice::from_ref(&inst), 0.0);
        let column_height = |x: usize| (0..h).filter(|&y| img.pixels[y * w + x] == 42).count();
        let cols: Vec<usize> = (0..w).filter(|&x| column_height(x) > 0).collect();
        assert!(cols.len() > 10, "the poster shows: {} columns", cols.len());
        let (first, last) = (cols[0], *cols.last().unwrap());
        assert_eq!((column_height(first), column_height(last)), (28, 24), "the nearer side taller");
        let zmid = |x: usize| zbuf[(h / 2) * w + x];
        assert!(zmid(first + 1) > zmid(last - 1), "1/z falls away from the eye");
        // Turned round, its vpn points at the eye: back-face culled.
        let mut img2 = Image::new(w, h, 9);
        let behind = SpriteInstance { angles: [0.0, 225.0, 0.0], ..inst };
        draw(&mut img2, &mut zbuf, cam_at_origin(), std::slice::from_ref(&behind), 0.0);
        assert!(img2.pixels.iter().all(|&p| p == 9), "back-face culled");
    }

    #[test]
    fn a_sprite_across_the_view_edge_is_clipped_to_it() {
        // A big poster off to the left: clipped to the view's side, its spans
        // start at column 0 and nothing is written past the view.
        let (w, h) = (80usize, 60usize);
        let mut img = Image::new(w, h, 9);
        let mut zbuf = vec![i16::MIN; w * h];
        let spr = sprite_of_type(SPR_VP_PARALLEL, 64, 64, 42);
        let inst =
            SpriteInstance { sprite: &spr, origin: [60.0, 70.0, 0.0], angles: [0.0; 3], frame: 0, models_before: 0 };
        draw(&mut img, &mut zbuf, cam_at_origin(), std::slice::from_ref(&inst), 0.0);
        let rows_at_left = (0..h).filter(|&y| img.pixels[y * w] == 42).count();
        assert!(rows_at_left > 20, "the clipped poster reaches column 0 on {rows_at_left} rows");
        // Behind the eye: nothing.
        let mut img2 = Image::new(w, h, 9);
        let back = SpriteInstance { origin: [-60.0, 0.0, 0.0], ..inst };
        draw(&mut img2, &mut zbuf, cam_at_origin(), std::slice::from_ref(&back), 0.0);
        assert!(img2.pixels.iter().all(|&p| p == 9));
    }

    #[test]
    fn group_frames_follow_r_get_spriteframe() {
        // Intervals 0.1, 0.3, 0.6: at 0.65 s (0.05 into the second loop) the
        // first, at 0.2 the second, at 0.59 the third.
        use crate::spr::Frame as F;
        let pic = |fill: u8| SpriteFrame { origin: [-1, 1], width: 2, height: 2, pixels: vec![fill; 4] };
        let mut spr = test_sprite(2, 2, 0);
        spr.frames = vec![F::Group { intervals: vec![0.1, 0.3, 0.6], frames: vec![pic(1), pic(2), pic(3)] }];
        let fill = |time: f32| sprite_frame(&spr, 0, time).map(|f| f.pixels[0]);
        assert_eq!((fill(0.65), fill(0.2), fill(0.59), fill(0.0)), (Some(1), Some(2), Some(3), Some(1)));
        // An index the sprite has not: frame 0.
        assert_eq!(sprite_frame(&spr, 7, 0.2).map(|f| f.pixels[0]), Some(2));
    }

    #[test]
    fn clip_sprite_face_keeps_the_inner_side() {
        // A unit square across the plane x = 0.5 (normal +X): the half with
        // x >= 0.5, the crossing points split in.
        let square = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]];
        let mut out = Vec::new();
        clip_sprite_face(&square, &([1.0, 0.0, 0.0], 0.5), &mut out);
        assert_eq!(out, vec![[0.5, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.5, 1.0, 0.0]]);
        // A point on the plane is kept and makes no new one.
        clip_sprite_face(&square, &([1.0, 0.0, 0.0], 1.0), &mut out);
        assert_eq!(out, vec![[1.0, 0.0, 0.0], [1.0, 1.0, 0.0]]);
    }
}
