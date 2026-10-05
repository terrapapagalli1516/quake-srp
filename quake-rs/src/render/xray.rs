//! EXTRA, not id, and debug only: the renderer's machinery made visible
//! (`quaketool film`'s x-ray views).
//!
//! id kept a few such views as cvars — `r_drawflat` painted each surface one
//! colour, `r_dspeeds` and `r_reportsurfout` counted — and `r_novis` drew
//! without the potentially visible set. This module is that idea for a film:
//! a [`Renderer`](super::Renderer) given [`XrayOptions`] keeps, for the main
//! view of each frame it draws, what id's passes decided on the way to its
//! pixels ([`XrayFrame`]) — every span the edge scan emitted and the surface
//! it belongs to, each surface's mip level and surface-cache block (and
//! whether the frame baked it), its texture gradients, the leaves the PVS
//! marked, the 16-bit `1/z` buffer before and after the entities, the alias
//! models' screen triangles and the brush entities' edges — and a host draws
//! whatever it likes from it. Two options change what is drawn, for a view
//! that shows something id's never did: [`XrayOptions::lightmaps`] (the
//! walls' light alone) and [`XrayOptions::vis_from`] (the PVS of another
//! point, as QuakeSpasm's `r_lockpvs`).
//!
//! None of it is on a normal path. A renderer without options (every host's,
//! [`Renderer::new`](super::Renderer::new)) tests one `Option` a frame and
//! one `bool` per surface it prepares, and draws id's pixels; the capture
//! itself runs after the frame's bands, on the calling thread, only when
//! asked.

use super::raster::PerspSpan;
use crate::bsp::Bsp;
use crate::math::{Vec3, dot, sub};

/// What an x-ray renderer does beyond id's frame (see the module doc).
/// [`Default`] does nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct XrayOptions {
    /// Keep the main view's [`XrayFrame`] each frame
    /// ([`Renderer::xray_frame`](super::Renderer::xray_frame)).
    pub capture: bool,
    /// Draw every lit wall as a single texel of this palette index, shaded by
    /// its lightmap through id's colormap — the light alone, no texture. The
    /// liquids and the sky are drawn as ever.
    pub lightmaps: Option<u8>,
    /// Mark the potentially visible set from the leaf holding this point
    /// instead of the eye's (`R_MarkLeaves`' `r_viewleaf`): a camera outside
    /// sees what that point's PVS lets the renderer walk, and nothing else. A
    /// point in solid (or a map without vis) marks every leaf, as id does.
    pub vis_from: Option<Vec3>,
    /// Keep the view's pixels as drawn and its world drawn again with exact
    /// perspective ([`XrayFrame::drawn`], [`XrayFrame::exact`]): which pixels
    /// the frame's perspective span puts off the exact texel. A second world
    /// pass, on the calling thread.
    pub exact: bool,
}

/// The projection of a frame's view (`R_ViewChanged`, `R_SetupFrame`): a
/// world point goes to the screen as the edge renderer projects a vertex,
/// with id's pixel centres on the integers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct XrayView {
    pub origin: Vec3,
    pub forward: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    /// `xcenter`, `ycenter`: the view's centre, pixel centres on the integers.
    pub xcenter: f32,
    pub ycenter: f32,
    pub xscale: f32,
    pub yscale: f32,
}

impl XrayView {
    /// `p` in view space: along `right`, `up` and `forward` (the depth).
    pub fn to_view(&self, p: Vec3) -> Vec3 {
        let d = sub(p, self.origin);
        [dot(d, self.right), dot(d, self.up), dot(d, self.forward)]
    }

    /// A view-space point (depth `v[2] > 0`) on the screen: `(x, y)` with
    /// pixel centres on the integers, and its `1/z` in the z-buffer's units
    /// ([`ZBUF_SCALE`] over the depth).
    pub fn screen(&self, v: Vec3) -> [f32; 3] {
        let zi = 1.0 / v[2];
        [self.xcenter + self.xscale * v[0] * zi, self.ycenter - self.yscale * v[1] * zi, ZBUF_SCALE * zi]
    }

    /// The four planes the view is clipped to (left, right, top, bottom),
    /// each `(normal, dist)` with the inside where `dot(normal, p) >= dist`,
    /// for a `w x h` view.
    pub fn frustum(&self, w: usize, h: usize) -> [(Vec3, f32); 4] {
        // The corner rays of the view's edges (pixel centres on the integers:
        // the view runs from -0.5 to w - 0.5).
        let ray = |x: f32, y: f32| {
            let (vx, vy) = ((x - self.xcenter) / self.xscale, (self.ycenter - y) / self.yscale);
            [
                self.forward[0] + vx * self.right[0] + vy * self.up[0],
                self.forward[1] + vx * self.right[1] + vy * self.up[1],
                self.forward[2] + vx * self.right[2] + vy * self.up[2],
            ]
        };
        let (l, r, t, b) = (-0.5, w as f32 - 0.5, -0.5, h as f32 - 0.5);
        let (tl, tr, bl, br) = (ray(l, t), ray(r, t), ray(l, b), ray(r, b));
        // Each side's plane through the eye and two corner rays, turned so
        // the view's centre ray is inside.
        let centre = ray((l + r) / 2.0, (t + b) / 2.0);
        let plane = |a: Vec3, c: Vec3| {
            let n = crate::math::cross(a, c);
            let n = if dot(n, centre) < 0.0 { [-n[0], -n[1], -n[2]] } else { n };
            (n, dot(n, self.origin))
        };
        [plane(tl, bl), plane(br, tr), plane(tr, tl), plane(bl, br)]
    }
}

/// The z-buffer's scale: id's 16-bit `1/z` is `(1/z * 0x8000 * 0x10000) >>
/// 16`, so a depth `z` is stored as about `32768 / z` (larger is nearer).
pub const ZBUF_SCALE: f32 = 32768.0;

/// Which model a surface is a face of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XrayModel {
    /// The background surface (`r_clearcolor`, behind everything).
    Background,
    /// The world, model 0 of its bsp.
    World,
    /// An inline brush entity: model `n` of the world's bsp (`*n`).
    Inline(usize),
    /// An external `b_*.bsp` box, the `n`-th of the frame's.
    External(usize),
}

/// How a surface's spans were painted (`D_DrawSurfaces`' branches).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XrayPaint {
    /// One colour: the background, a face seen edge-on.
    Fill,
    /// The sky (`D_DrawSkyScans8`).
    Sky,
    /// A liquid (`Turbulent8`).
    Liquid,
    /// A wall from its surface-cache block (`D_DrawSpans16` and friends).
    Cached,
    /// A wall lit per pixel (no block), or the port's flat colour, or the
    /// lightmaps alone ([`XrayOptions::lightmaps`]).
    PerPixel,
}

/// A face's screen-plane gradients of `1/z`, `s/z` and `t/z` (texels of mip
/// level 0, `s`/`t` relative to the eye's) and the eye's `(s, t)`: each
/// gradient is `o + dx * x + dy * y` at the screen point `(x, y)`, pixel
/// `(u, v)`'s centre being `(u + 0.5, v + 0.5)` — the renderer's own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct XrayGrads {
    pub zi: [f64; 3],
    pub sz: [f64; 3],
    pub tz: [f64; 3],
    pub st_eye: [f64; 2],
}

impl XrayGrads {
    fn at(l: &[f64; 3], x: f64, y: f64) -> f64 {
        l[0] + l[1] * x + l[2] * y
    }

    /// The exact texel coordinates `(s, t)` (mip 0) at the screen point
    /// `(x, y)`: the perspective divide at that point.
    pub fn st(&self, x: f64, y: f64) -> [f64; 2] {
        let zi = Self::at(&self.zi, x, y);
        if zi.abs() < 1e-12 {
            return self.st_eye;
        }
        [Self::at(&self.sz, x, y) / zi + self.st_eye[0], Self::at(&self.tz, x, y) / zi + self.st_eye[1]]
    }

    /// `1/z` at the screen point `(x, y)`.
    pub fn zi(&self, x: f64, y: f64) -> f64 {
        Self::at(&self.zi, x, y)
    }
}

/// One surface of the frame (`surf_t`), as its spans were drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct XraySurface {
    pub model: XrayModel,
    /// The face, in its model's bsp (the world's for [`XrayModel::World`]
    /// and [`XrayModel::Inline`]).
    pub face: usize,
    pub paint: XrayPaint,
    /// The front-to-back key the BSP walk gave it (smaller is nearer).
    pub key: i32,
    /// The mip level its block was baked at (`D_MipLevelForScale`), for a
    /// cached wall.
    pub mip: Option<u32>,
    /// Its block's size in texels of that level, for a cached wall.
    pub block: Option<(usize, usize)>,
    /// Whether this frame baked its block (a miss: new, or its light
    /// changed), for a cached wall.
    pub baked: bool,
    /// Its texture gradients, for a wall or a liquid.
    pub grads: Option<XrayGrads>,
}

/// One span of the edge scan: `count` pixels of row `v` from column `u`, of
/// surface [`XraySpan::surface`] (an index into [`XrayFrame::surfaces`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XraySpan {
    pub v: u32,
    pub u: u32,
    pub count: u32,
    pub surface: u32,
}

/// One alias-model triangle as the rasteriser got it: three screen vertices
/// `(x, y, 1/z in z-buffer units)` (the gun's `1/z` tripled, as id draws it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct XrayTriangle {
    pub v: [[f32; 3]; 3],
    pub gun: bool,
}

/// What a frame's main view was made of (see the module doc).
#[derive(Clone, Debug, Default)]
pub struct XrayFrame {
    /// The view's size.
    pub w: usize,
    pub h: usize,
    /// Its projection, `None` before a frame was drawn.
    pub view: Option<XrayView>,
    /// The perspective span the walls and liquids were drawn with.
    pub persp: PerspSpan,
    /// Every span, row after row and left to right: every pixel of the view
    /// is in exactly one.
    pub spans: Vec<XraySpan>,
    /// The surfaces the spans name (index 0 unused).
    pub surfaces: Vec<Option<XraySurface>>,
    /// The world's `1/z` (`D_DrawZSpans`) and the finished frame's, after
    /// the alias models, sprites and the gun: where they differ an entity
    /// was drawn.
    pub world_z: Vec<i16>,
    pub zbuf: Vec<i16>,
    /// The alias models' triangles (and the gun's).
    pub triangles: Vec<XrayTriangle>,
    /// The brush entities' face edges in world space (doors, lifts, the
    /// item boxes), each `[from, to]`.
    pub brush_edges: Vec<[Vec3; 2]>,
    /// The world's leaves the PVS marked (`mleaf_t.visframe` current).
    pub leaf_visible: Vec<bool>,
    /// With [`XrayOptions::exact`]: the view's pixels as the frame drew them
    /// (entities and all), and its world's spans drawn again with exact
    /// perspective ([`PerspSpan::Exact`]) from the same surface-cache blocks.
    /// Where no entity covers a pixel, the two differ only where the frame's
    /// span read another texel than the exact one.
    pub drawn: Vec<u8>,
    pub exact: Vec<u8>,
    /// The main view's row bands as the renderer's threads drew them:
    /// `(first row, end row, thread)`, threads numbered as they started
    /// (which thread takes which band is the scheduler's: the pixels are the
    /// same whichever does), and how many threads drew the frame.
    pub bands: Vec<(u32, u32, u32)>,
    pub threads: u32,
}

impl XrayFrame {
    /// The surface each pixel of the view belongs to (an index into
    /// [`XrayFrame::surfaces`]; 0 where no span reached, never in a frame).
    pub fn surface_map(&self) -> Vec<u32> {
        let mut map = vec![0u32; self.w * self.h];
        for sp in &self.spans {
            let row = sp.v as usize * self.w;
            let (u, n) = (sp.u as usize, sp.count as usize);
            if let Some(px) = map.get_mut(row + u..row + u + n) {
                px.fill(sp.surface);
            }
        }
        map
    }

    /// The span each pixel of the view belongs to (an index into
    /// [`XrayFrame::spans`]).
    pub fn span_map(&self) -> Vec<u32> {
        let mut map = vec![u32::MAX; self.w * self.h];
        for (i, sp) in self.spans.iter().enumerate() {
            let row = sp.v as usize * self.w;
            let (u, n) = (sp.u as usize, sp.count as usize);
            if let Some(px) = map.get_mut(row + u..row + u + n) {
                px.fill(i as u32);
            }
        }
        map
    }
}

/// The world-space edges of model `model` of `bsp` placed at `origin` and
/// turned by `rotation` (`world::entity_rotation_matrix`'s), into `out`.
pub(super) fn model_edges(bsp: &Bsp, model: usize, origin: Vec3, rotation: &[[f32; 3]; 3], out: &mut Vec<[Vec3; 2]>) {
    let Some(m) = bsp.models.get(model) else { return };
    let first = m.firstface.max(0) as usize;
    let end = (first + m.numfaces.max(0) as usize).min(bsp.faces.len());
    let place = |p: Vec3| {
        let r = super::world::entity_rotate_transpose(rotation, p);
        [r[0] + origin[0], r[1] + origin[1], r[2] + origin[2]]
    };
    for face in &bsp.faces[first..end] {
        for k in 0..face.numedges.max(0) as usize {
            let Some(&se) = bsp.surfedges.get(face.firstedge.max(0) as usize + k) else { continue };
            let Some(e) = bsp.edges.get(se.unsigned_abs() as usize) else { continue };
            let (Some(a), Some(b)) = (bsp.vertexes.get(e.v[0] as usize), bsp.vertexes.get(e.v[1] as usize)) else {
                continue;
            };
            out.push([place(a.point), place(b.point)]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{Camera, Image, NEUTRAL_LIGHTSTYLE_SCALES, PerspSpan, Renderer, Scene, parse_palette};

    /// Render `scene` with `options` (none: id's renderer) on `threads`.
    fn draw(scene: &Scene, options: Option<XrayOptions>, threads: usize) -> (Image, Option<XrayFrame>) {
        let mut r = Renderer::new();
        r.set_threads(threads);
        r.set_xray(options);
        let img = r.render(scene);
        (img, r.xray_frame().cloned())
    }

    const CAPTURE: XrayOptions = XrayOptions { capture: true, lightmaps: None, vis_from: None, exact: false };

    /// The shareware pak's map `name`, its palette and colormap, when the pak is here.
    fn map(name: &str) -> Option<(Bsp, [[u8; 3]; 256], Vec<u8>)> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../quake-data/ID1/PAK0.PAK");
        let Ok(pak) = crate::pak::Pak::open(&path) else {
            eprintln!("skipped: no shareware pak at {}", path.display());
            return None;
        };
        let read = |n: &str| pak.read_file(n).expect("read").expect(n);
        let bsp = Bsp::parse(&read(&format!("maps/{name}.bsp"))).expect("a bsp");
        Some((bsp, parse_palette(&read("gfx/palette.lmp")).expect("palette"), read("gfx/colormap.lmp")))
    }

    #[test]
    fn a_capture_changes_no_pixel_and_every_pixel_is_in_one_span() {
        let room = crate::render::demo_room();
        let pal = [[0u8; 3]; 256];
        let cam = Camera::looking_at([0.0, -100.0, 40.0], [0.0, 100.0, 30.0], 90.0);
        let scene = Scene::new(&room, cam, 97, 61, &pal);
        let (plain, none) = draw(&scene, None, 1);
        assert!(none.is_none(), "no capture without options");
        for threads in [1, 3] {
            let (img, x) = draw(&scene, Some(CAPTURE), threads);
            assert_eq!(img, plain, "the capture draws id's pixels");
            let x = x.expect("a capture");
            assert_eq!((x.w, x.h), (97, 61));
            let mut hits = vec![0u32; 97 * 61];
            for sp in &x.spans {
                for k in 0..sp.count {
                    hits[(sp.v * 97 + sp.u + k) as usize] += 1;
                }
                assert!(x.surfaces[sp.surface as usize].is_some(), "a span's surface is kept");
            }
            assert!(hits.iter().all(|&n| n == 1), "every pixel in exactly one span");
            assert_eq!(x.world_z, x.zbuf, "no entity: the z-buffer is the world's");
            // The bands: every row once, by as many threads as drew it.
            let mut rows = vec![0u32; 61];
            for &(y0, y1, thread) in &x.bands {
                assert!(thread < x.threads, "{thread} of {}", x.threads);
                for r in &mut rows[y0 as usize..y1 as usize] {
                    *r += 1;
                }
            }
            assert!(rows.iter().all(|&n| n == 1), "every row in one band: {rows:?}");
            assert_eq!(x.threads as usize, threads);
            assert!(x.surfaces.iter().flatten().any(|s| s.model == XrayModel::World));
        }
    }

    #[test]
    fn a_capture_projects_as_the_renderer_does() {
        // A world point on the camera's axis lands at the view's centre, its
        // 1/z in the z-buffer's units.
        let room = crate::render::demo_room();
        let pal = [[0u8; 3]; 256];
        let cam = Camera::looking_at([0.0, -100.0, 40.0], [0.0, 100.0, 40.0], 90.0);
        let (_, x) = draw(&Scene::new(&room, cam, 100, 60, &pal), Some(CAPTURE), 1);
        let x = x.unwrap();
        let view = x.view.unwrap();
        let [sx, sy, zb] = view.screen(view.to_view([0.0, 0.0, 40.0]));
        assert!((sx - 49.5).abs() < 1e-3 && (sy - 29.5).abs() < 1e-3, "{sx},{sy}");
        assert!((zb - ZBUF_SCALE / 100.0).abs() < 1e-2, "{zb}");
        // The frustum holds the centre ray and not a point behind the eye.
        let inside = |p: Vec3| view.frustum(100, 60).iter().all(|(n, d)| dot(*n, p) >= *d - 1e-3);
        assert!(inside([0.0, 0.0, 40.0]));
        assert!(!inside([0.0, -200.0, 40.0]));
    }

    #[test]
    fn the_exact_pass_is_the_frame_drawn_exactly() {
        // A grazing view down e1m1's first corridor at id's 16-pixel spans:
        // the capture's exact pass is the world an exact renderer draws, its
        // `drawn` the frame itself, and the two differ (the spans' error).
        let Some((bsp, pal, cm)) = map("e1m1") else { return };
        let cam = Camera { pos: [480.0, -352.0, 110.0], yaw: 75.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let mut styles = NEUTRAL_LIGHTSTYLE_SCALES;
        styles[0] = 264.0 / 256.0;
        let at = |persp| Scene {
            colormap: Some(&cm),
            light_styles: &styles,
            options: crate::render::RenderOptions { persp_span: persp, ..Default::default() },
            ..Scene::new(&bsp, cam, 320, 200, &pal)
        };
        let (exact, _) = draw(&at(PerspSpan::Exact), None, 1);
        for threads in [1, 4] {
            let (img, x) = draw(&at(PerspSpan::Spans16), Some(XrayOptions { exact: true, ..CAPTURE }), threads);
            let x = x.unwrap();
            assert_eq!(x.drawn, img.pixels, "the view as drawn");
            assert_eq!(x.exact, exact.pixels, "the world drawn exactly");
            let off = x.drawn.iter().zip(&x.exact).filter(|(a, b)| a != b).count();
            assert!(off > 100, "id's spans put pixels off exact here: {off}");
        }
        let (_, x) = draw(&at(PerspSpan::Exact), Some(XrayOptions { exact: true, ..CAPTURE }), 2);
        let x = x.unwrap();
        assert_eq!(x.drawn, x.exact, "exact is exact");
    }

    #[test]
    fn the_lightmaps_view_and_another_points_pvs() {
        let Some((bsp, pal, cm)) = map("e1m1") else { return };
        let cam = Camera { pos: [544.0, 288.0, 80.0], yaw: 60.0, pitch: 5.0, roll: 0.0, fov_deg: 90.0 };
        let mut styles = NEUTRAL_LIGHTSTYLE_SCALES;
        styles[0] = 264.0 / 256.0;
        let scene = Scene { colormap: Some(&cm), light_styles: &styles, ..Scene::new(&bsp, cam, 160, 100, &pal) };
        let (plain, _) = draw(&scene, None, 1);
        let (lit, x) = draw(&scene, Some(XrayOptions { lightmaps: Some(9), ..CAPTURE }), 2);
        assert_ne!(lit, plain, "the light alone is another picture");
        // Every pixel of a lit wall is the grey's shade in some colormap row.
        let x = x.unwrap();
        let shades: Vec<u8> = (0..64).map(|row| cm[row * 256 + 9]).collect();
        let map = x.surface_map();
        let walls = (0..160 * 100)
            .filter(|&i| x.surfaces[map[i] as usize].is_some_and(|s| s.paint == XrayPaint::PerPixel))
            .collect::<Vec<_>>();
        assert!(walls.len() > 160 * 50, "most of the view is wall: {}", walls.len());
        assert!(walls.iter().all(|&i| shades.contains(&lit.pixels[i])));
        // The PVS of the start, from a camera far across the map: fewer
        // leaves than the camera's own, and the start's leaf among them.
        let start = [480.0, -352.0, 88.0];
        let (_, own) = draw(&scene, Some(CAPTURE), 1);
        let (_, other) = draw(&scene, Some(XrayOptions { vis_from: Some(start), ..CAPTURE }), 1);
        let (own, other) = (own.unwrap(), other.unwrap());
        assert_ne!(own.leaf_visible, other.leaf_visible);
        let leaf = crate::render::point_in_leaf(&bsp, start).unwrap();
        assert!(other.leaf_visible[leaf], "the start's own leaf");
        // From solid, every leaf (id's no-PVS rule).
        let (_, solid) = draw(&scene, Some(XrayOptions { vis_from: Some([0.0, 0.0, -10000.0]), ..CAPTURE }), 1);
        assert!(
            solid.unwrap().leaf_visible.iter().skip(1).filter(|&&v| v).count()
                > own.leaf_visible.iter().filter(|&&v| v).count()
        );
    }
}
