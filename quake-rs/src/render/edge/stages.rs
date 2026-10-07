//! The stages of one frame of the world, as text: what `oracle/pixel_trace.py`
//! puts beside id's (`oracle_stages`, `oracle/c/stages_oracle.c`) to find the
//! first stage a differing pixel's values part at. `quaketool view --stages`.
//!
//! A record a line, in the C's format: `F` the frame (the view axes,
//! `R_ViewChanged`'s numbers, the `screenedge` and clip planes, `skytime`),
//! `E` every edge the scan starts with, `S` every surface with a span, `G`
//! what `D_CalcGradients` left each textured surface's span routine (with a
//! hash of the block it reads), `P` every span. A float is its IEEE bits in
//! hex; a surface is named by its model (`world`, `*N`, `ext@x,y,z`) and its
//! face's index in that model's bsp. Debugging only: nothing is recorded
//! unless [`Renderer::set_stages`](crate::render::Renderer::set_stages) asked.

use std::fmt::Write as _;

use super::super::raster::SurfGrads;
use super::super::sky::sky_time;
use super::super::surf::Bakes;
use super::{BACKGROUND, EdgeState, Ent, NONE, Paint, WorldDraw};
use crate::math::{Vec3, sub};

/// What the stages need besides the edge state: the frame's text so far, and
/// each surface's name and eye (`transformed_modelorg`) from the frame's
/// models, which only [`EdgeState::build`] has.
#[derive(Default)]
pub(in crate::render) struct Stages {
    pub(in crate::render) text: String,
    ids: Vec<String>,
    eyes: Vec<Vec3>,
}

/// ` tag=bits`: a float's IEEE bits, as the C's `%08x` of them.
fn f32_field(out: &mut String, tag: &str, f: f32) {
    let _ = write!(out, " {tag}={:08x}", f.to_bits());
}

fn vec_field(out: &mut String, tag: &str, v: Vec3) {
    for (i, c) in v.iter().enumerate() {
        f32_field(out, &format!("{tag}{i}"), *c);
    }
}

impl EdgeState {
    /// Name every surface of the frame (and its eye along the frame's axes,
    /// `D_DrawSurfaces`' `TransformVector (local_modelorg)`), then record the
    /// edges the scan starts with: `R_ScanEdges`' `newedges`, row by row.
    pub(super) fn stage_edges(&mut self, ents: &[Ent]) {
        let Some(mut st) = self.stages.take() else { return };
        let gview = self.grad_view();
        st.ids = self.surfs.iter().enumerate().map(|(si, s)| self.surface_name(si, s.ent, s.face, ents)).collect();
        st.eyes = self.surfs.iter().map(|s| gview.transform(sub(self.r_origin, ents[s.ent as usize].origin))).collect();
        let name = |s: u32| if s == 0 { "-" } else { st.ids[s as usize].as_str() };
        let mut text = std::mem::take(&mut st.text);
        for (v, &head) in self.newedges.iter().enumerate() {
            let mut e = head;
            while e != NONE {
                let edge = &self.edges[e as usize];
                let _ =
                    write!(text, "E {v} {} {} {} {}", edge.u, edge.u_step, name(edge.surfs[0]), name(edge.surfs[1]));
                f32_field(&mut text, "nearzi", edge.nearzi);
                let _ = writeln!(text, " last={}", edge.last);
                e = edge.next;
            }
        }
        st.text = text;
        self.stages = Some(st);
    }

    /// `ent:face`: `world`, `*N` (an inline brush model) or `ext@x,y,z` (a
    /// `b_*.bsp` box at its origin, truncated as the C's `(int)`), and the
    /// face's index in its bsp; `bg` for the background.
    fn surface_name(&self, si: usize, ent: u32, face: u32, ents: &[Ent]) -> String {
        if si == BACKGROUND as usize {
            return "bg".into();
        }
        match ents.get(ent as usize) {
            Some(_) if ent == 0 => format!("world:{face}"),
            Some(e) if e.world_bsp => format!("*{}:{face}", e.model),
            Some(e) => format!("ext@{},{},{}:{face}", e.origin[0] as i32, e.origin[1] as i32, e.origin[2] as i32),
            None => format!("?{ent}:{face}"),
        }
    }

    /// Every surface that owns a span (`S`) and its spans (`P`), as
    /// `D_DrawSurfaces` meets them.
    pub(super) fn stage_surfaces(&mut self, world: &WorldDraw) {
        let Some(mut st) = self.stages.take() else { return };
        for (si, s) in self.surfs.iter().enumerate() {
            if !s.has_spans || si == 0 {
                continue;
            }
            let _ = write!(st.text, "S {} key={} flags={}", st.ids[si], s.key, s.flags);
            f32_field(&mut st.text, "nearzi", s.nearzi);
            f32_field(&mut st.text, "ziorigin", s.d_ziorigin);
            f32_field(&mut st.text, "zistepu", s.d_zistepu);
            f32_field(&mut st.text, "zistepv", s.d_zistepv);
            st.text.push('\n');
        }
        for (v, row) in world.rows.windows(2).enumerate() {
            for sp in &world.spans[row[0] as usize..row[1] as usize] {
                let _ = writeln!(st.text, "P {v} {} {} {}", sp.u, sp.count, st.ids[sp.surf as usize]);
            }
        }
        self.stages = Some(st);
    }

    /// Once the blocks are baked: each textured surface's gradients (`G`),
    /// then the frame (`F`) at `cl.time` `time`. The edge state is the last
    /// view's: the stages are a one-view frame's (`quaketool view`).
    pub(in crate::render) fn stage_grads(&mut self, world: &WorldDraw, bakes: &Bakes, time: f64) {
        let Some(mut st) = self.stages.take() else { return };
        let fnv = |rows: &mut dyn Iterator<Item = u8>| {
            rows.fold(2_166_136_261u32, |h, b| (h ^ u32::from(b)).wrapping_mul(16_777_619))
        };
        for (si, sd) in world.surfs.iter().enumerate() {
            let Some(sd) = sd else { continue };
            let (kind, mip, g, w, h, hash) = match &sd.paint {
                Paint::Cached { grads, block, job } => {
                    let texels = job.map_or(&block.block[..], |job| bakes.block(job));
                    let hash = fnv(&mut texels.iter().copied().take(block.bw * block.bh));
                    ("spans", block.mip, grads, block.bw, block.bh, hash)
                }
                Paint::Turb { grads, mt } => {
                    let (w, h) = (mt.width as usize, (mt.height as usize).min(64));
                    ("turb", 0, grads, w, h, fnv(&mut mt.pixels.iter().copied().take(w * h)))
                }
                _ => continue,
            };
            let SurfGrads { sdivz, tdivz, sadjust, tadjust, bbextents, bbextentt, .. } = *g;
            let _ = write!(st.text, "G {} {kind} mip={mip}", st.ids[si]);
            vec_field(&mut st.text, "tmo", st.eyes[si]);
            for (tag, v) in [
                ("sdivzorigin", sdivz.origin),
                ("sdivzstepu", sdivz.stepu),
                ("sdivzstepv", sdivz.stepv),
                ("tdivzorigin", tdivz.origin),
                ("tdivzstepu", tdivz.stepu),
                ("tdivzstepv", tdivz.stepv),
            ] {
                f32_field(&mut st.text, tag, v);
            }
            let _ = writeln!(
                st.text,
                " sadjust={sadjust} tadjust={tadjust} bbextents={bbextents} bbextentt={bbextentt} block={w}x{h} hash={hash:08x}"
            );
        }
        st.text.push('F');
        vec_field(&mut st.text, "vpn", self.vpn);
        vec_field(&mut st.text, "vright", self.vright);
        vec_field(&mut st.text, "vup", self.vup);
        for (tag, v) in [
            ("xcenter", self.xcenter),
            ("ycenter", self.ycenter),
            ("xscale", self.xscale),
            ("yscale", self.yscale),
            ("xscaleinv", self.xscaleinv),
            ("yscaleinv", self.yscaleinv),
            ("hfov", self.hfov),
            ("skytime", sky_time(time)),
        ] {
            f32_field(&mut st.text, tag, v);
        }
        for i in 0..4 {
            vec_field(&mut st.text, &format!("edge{i}_"), self.screenedge[i]);
            vec_field(&mut st.text, &format!("clip{i}_"), self.clip[i].normal);
            f32_field(&mut st.text, &format!("clip{i}_dist"), self.clip[i].dist);
        }
        st.text.push('\n');
        self.stages = Some(st);
    }
}

#[cfg(test)]
mod tests {
    use crate::render::{Camera, Renderer, Scene, demo_room};

    /// A frame's stages: one frame record, and spans that cover every pixel
    /// of the view exactly once, each naming a surface that has its record;
    /// the textured ones with their gradients. Nothing when not asked.
    #[test]
    fn the_stages_name_every_span_and_its_surface() {
        let bsp = demo_room();
        let pal = [[0u8; 3]; 256];
        let cam = Camera { pos: [0.0, 0.0, 48.0], yaw: 30.0, pitch: -10.0, roll: 0.0, fov_deg: 90.0 };
        let (w, h) = (64usize, 40usize);
        let scene = Scene::new(&bsp, cam, w, h, &pal);
        let mut r = Renderer::new();
        r.render(&scene);
        assert_eq!(r.take_stages(), None, "not recorded unless asked");
        r.set_stages(true);
        r.render(&scene);
        let text = r.take_stages().expect("recorded");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.iter().filter(|l| l.starts_with("F ")).count(), 1);
        let surfaces: std::collections::HashSet<&str> =
            lines.iter().filter_map(|l| l.strip_prefix("S ")).filter_map(|l| l.split(' ').next()).collect();
        let mut covered = vec![0u8; w * h];
        for l in lines.iter().filter_map(|l| l.strip_prefix("P ")) {
            let f: Vec<&str> = l.split(' ').collect();
            let (v, u, n): (usize, usize, usize) =
                (f[0].parse().unwrap(), f[1].parse().unwrap(), f[2].parse().unwrap());
            assert!(surfaces.contains(f[3]), "span of {} with no surface record", f[3]);
            for c in &mut covered[v * w + u..v * w + u + n] {
                *c += 1;
            }
        }
        assert!(covered.iter().all(|&c| c == 1), "every pixel in exactly one span");
        assert!(lines.iter().any(|l| l.starts_with("E ")), "the edges");
        assert_eq!(r.take_stages().as_deref(), Some(""), "taken");
    }
}
