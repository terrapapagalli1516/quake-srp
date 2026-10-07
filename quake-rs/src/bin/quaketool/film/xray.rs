//! The x-ray views: a frame's 3-D view redrawn from what the renderer
//! decided on the way to it ([`quake_rs::render::xray::XrayFrame`]), for a
//! film about how it works.
//!
//! The base picture (`xray MODE`):
//!
//! | mode | each pixel of the 3-D view |
//! |---|---|
//! | `game` | the game's own |
//! | `black` | black (for a wireframe alone) |
//! | `z` | its 16-bit `1/z`, the z-buffer the entities test, as grey: near is light |
//! | `surfaces` | one palette colour per surface, as id's `r_drawflat` painted them |
//! | `spans` | one colour per span: the rows the edge scan cut each surface into |
//! | `segments` | each span's affine runs in rust and brown in turn over the picture's grey, and a lava-orange tick at each perspective divide: every 16 pixels (id's), 8 (slop's), none at exact |
//! | `mip` | the mip level each wall was drawn at: 0 green, 1 yellow, 2 orange, 3 red, over the picture's grey |
//! | `cache` | the surface-cache block each wall was drawn from, a tile each, darker for a coarser mip, with black borders |
//! | `bakes` | the blocks this frame baked (new, or their light changed: a flickering light rebakes what it lights), lit |
//! | `luxels` | the picture with each wall's lightmap cells outlined: a light sample every 16 texels |
//! | `leaves` | the BSP leaf the visible point is in, a colour each |
//! | `lightmaps` | the walls' light alone (the renderer draws it: `XrayOptions::lightmaps`) |
//! | `error` | how far the affine spans' texel is from the exact one, in texels: none dark, half a texel yellow, one or more red |
//!
//! Where an entity covers the world (an alias model, a sprite, the gun) the
//! machinery views show the game's pixels: those are drawn after the edge
//! scan, against the z-buffer.
//!
//! The wireframe (`wire`) is drawn over any of them: the world's polygon
//! edges, the brush entities' and the alias models' triangles, anti-aliased,
//! each tested against the z-buffer (`hidden`) or not (`through`); with
//! `pvs`, the world's edges are coloured by what the renderer did with their
//! face — drawn (has a span), in the PVS but not drawn (hidden or outside
//! the view), or outside the PVS; with `culled`, only the last. The colours
//! are Quake palette entries.

use quake_rs::bsp::Bsp;
use quake_rs::render::xray::{XrayFrame, XrayGrads, XrayModel, XrayPaint, XraySurface, XrayView, ZBUF_SCALE};
use quake_rs::render::{PerspSpan, point_in_leaf};

use super::shot::{Wire, XrayBase};

/// An RGB picture.
pub struct Rgb<'a> {
    pub w: usize,
    pub h: usize,
    pub px: &'a mut [u8],
}

/// The film's colours, as Quake palette indices (the director's).
const RUST: usize = 105;
const BROWN: usize = 26;
const LIGHT_BROWN: usize = 31;
const LAVA: usize = 235;
const DEEP_LAVA: usize = 233;
const FLAME: usize = 238;
const LIGHT_SLATE: usize = 40;
const BLUE: usize = 244;

/// A palette colour for `key` from Quake's palette, avoiding the darkest
/// and the fullbright rows: rows 1..14 of 16, each row's brighter half.
fn palette_colour(palette: &[[u8; 3]; 256], key: u64) -> [u8; 3] {
    let h = key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40;
    let row = 1 + (h % 13) as usize;
    let col = 2 + ((h >> 8) % 6) as usize;
    palette[row * 16 + col]
}

fn luma(c: [u8; 3]) -> f32 {
    0.299 * f32::from(c[0]) + 0.587 * f32::from(c[1]) + 0.114 * f32::from(c[2])
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2].map(|k| (f32::from(a[k]) + (f32::from(b[k]) - f32::from(a[k])) * t).round() as u8)
}

fn grey(v: f32) -> [u8; 3] {
    let v = v.clamp(0.0, 255.0) as u8;
    [v, v, v]
}

/// The texels a pixel of `s` is read in: mip `level`'s.
fn texel_scale(s: &XraySurface) -> f64 {
    f64::from(1u32 << s.mip.unwrap_or(0).min(3))
}

/// Redraw the 3-D view at `(vx, vy)` of `screen` (the game's picture, at
/// the mode's size) as `base` says, blended over the game's picture by
/// `strength` (0: the game's alone). The wireframe is drawn later, over the
/// output frame ([`wire_over`]).
#[allow(clippy::too_many_arguments)]
pub fn composite(
    screen: &mut Rgb,
    (vx, vy): (usize, usize),
    x: &XrayFrame,
    bsp: &Bsp,
    palette: &[[u8; 3]; 256],
    base: XrayBase,
    tint: &TintRgb,
    strength: f64,
) {
    let Some(view) = x.view else { return };
    let (w, h) = (x.w, x.h);
    if strength <= 0.0 || w == 0 || h == 0 || vx + w > screen.w || vy + h > screen.h {
        return;
    }
    // The game's picture of the view, which every mode starts from.
    let mut game = vec![[0u8; 3]; w * h];
    for y in 0..h {
        for xx in 0..w {
            let at = ((vy + y) * screen.w + vx + xx) * 3;
            game[y * w + xx] = [screen.px[at], screen.px[at + 1], screen.px[at + 2]];
        }
    }
    if base == XrayBase::Game {
        return;
    }
    let out = base_picture(x, bsp, palette, base, &view, &game, tint);
    let t = strength.clamp(0.0, 1.0) as f32;
    for y in 0..h {
        for xx in 0..w {
            let at = ((vy + y) * screen.w + vx + xx) * 3;
            let c = mix(game[y * w + xx], out[y * w + xx], t);
            screen.px[at..at + 3].copy_from_slice(&c);
        }
    }
}

/// The base picture of the view.
fn base_picture(
    x: &XrayFrame,
    bsp: &Bsp,
    palette: &[[u8; 3]; 256],
    base: XrayBase,
    view: &XrayView,
    game: &[[u8; 3]],
    tint: &TintRgb,
) -> Vec<[u8; 3]> {
    let (w, h) = (x.w, x.h);
    match base {
        XrayBase::Game | XrayBase::Lightmaps => return game.to_vec(),
        XrayBase::Black => return vec![[0; 3]; w * h],
        XrayBase::Z => {
            // Depth on a log scale from 16 to 1024 units: near is light.
            return x
                .zbuf
                .iter()
                .map(|&zb| {
                    let z = ZBUF_SCALE / f32::from(zb.max(1));
                    let t = ((z.max(16.0) / 16.0).ln() / 64.0f32.ln()).clamp(0.0, 1.0);
                    grey(255.0 * (1.0 - t).powf(1.5))
                })
                .collect();
        }
        _ => {}
    }
    let mut out = game.to_vec();
    if base == XrayBase::Bands {
        // Each thread's bands in its own colour over the picture, a dark
        // line where one band meets the next.
        const THREAD: [usize; 8] = [LAVA, BLUE, 63, 251, 111, 208, 40, 183];
        for &(y0, y1, thread) in &x.bands {
            let c = palette[THREAD[thread as usize % THREAD.len()]];
            for y in y0 as usize..(y1 as usize).min(h) {
                for px in &mut out[y * w..(y + 1) * w] {
                    *px = if y == y0 as usize && y0 > 0 { [0, 0, 0] } else { mix(*px, c, 0.45) };
                }
            }
        }
        return out;
    }
    let entity = |i: usize| x.zbuf.get(i) != x.world_z.get(i);
    // Where a pixel's surface differs from its right or lower neighbour's.
    let surfaces = if base == XrayBase::Cache { x.surface_map() } else { Vec::new() };
    let surface_edge = |i: usize| {
        let s = surfaces[i];
        (i % w + 1 < w && surfaces[i + 1] != s) || (i + w < surfaces.len() && surfaces[i + w] != s)
    };
    let span_px = x.persp.pixels().max(1) as usize;
    for (si, sp) in x.spans.iter().enumerate() {
        let Some(Some(s)) = x.surfaces.get(sp.surface as usize) else { continue };
        let row = sp.v as usize * w;
        let surf_key = match s.model {
            XrayModel::Background => 0,
            XrayModel::World => s.face as u64 + 1,
            XrayModel::Inline(m) => (m as u64) << 32 | s.face as u64,
            XrayModel::External(e) => (e as u64 + 1) << 48 | s.face as u64,
        };
        for k in 0..sp.count as usize {
            let xx = sp.u as usize + k;
            let i = row + xx;
            if i >= out.len() || entity(i) {
                continue;
            }
            let g = game[i];
            let (px, py) = (xx as f64 + 0.5, sp.v as f64 + 0.5);
            out[i] = match base {
                XrayBase::Surfaces => {
                    if s.model == XrayModel::Background {
                        [0; 3]
                    } else {
                        palette_colour(palette, surf_key)
                    }
                }
                // The surface's colour, every other span of a row darker:
                // each surface is cut into one span a row.
                XrayBase::Spans => {
                    let c = palette_colour(palette, surf_key);
                    if (sp.v + si as u32) % 2 == 1 { mix(c, [0, 0, 0], 0.45) } else { c }
                }
                XrayBase::Segments => {
                    if !matches!(s.paint, XrayPaint::Cached | XrayPaint::PerPixel | XrayPaint::Liquid) {
                        grey(luma(g) * 0.6)
                    } else if span_px == 1 {
                        // Exact: every pixel its own divide.
                        palette[if k % 2 == 0 { LAVA } else { DEEP_LAVA }]
                    } else if is_divide(k, sp.count as usize, span_px) {
                        // The perspective divide: this pixel's texel is exact.
                        palette[LAVA]
                    } else {
                        // The affine run, rust and brown in turn over the picture.
                        let c = palette[if (k / span_px) % 2 == 0 { RUST } else { BROWN }];
                        mix(grey(luma(g)), c, 0.6)
                    }
                }
                XrayBase::Mip => match (s.paint, s.mip) {
                    (XrayPaint::Cached, Some(m)) => {
                        const MIP: [[u8; 3]; 4] = [[64, 200, 64], [230, 220, 60], [240, 140, 40], [220, 50, 40]];
                        mix(grey(luma(g)), MIP[m.min(3) as usize], 0.6)
                    }
                    _ => grey(luma(g) * 0.5),
                },
                XrayBase::Cache => match (s.paint, s.mip) {
                    // A tile a block, darker for a coarser mip, black where
                    // the block meets another.
                    (XrayPaint::Cached, Some(m)) => {
                        let c = palette_colour(palette, surf_key);
                        let c = mix(c, [0, 0, 0], 0.2 * m.min(3) as f32);
                        if surface_edge(i) { [0, 0, 0] } else { c }
                    }
                    _ => grey(luma(g) * 0.3),
                },
                XrayBase::Bakes => match s.paint {
                    XrayPaint::Cached if s.baked => mix(grey(luma(g)), palette[FLAME], 0.6),
                    _ => grey(luma(g) * 0.5),
                },
                XrayBase::Luxels => match (s.paint, s.grads) {
                    (XrayPaint::Cached | XrayPaint::PerPixel, Some(gr)) if on_grid(&gr, px, py, 16.0) => {
                        mix(g, palette[FLAME], 0.7)
                    }
                    _ => g,
                },
                XrayBase::Leaves => match (s.paint, s.grads) {
                    (XrayPaint::Sky | XrayPaint::Fill, _) | (_, None) => grey(luma(g) * 0.3),
                    (_, Some(gr)) => {
                        let zi = gr.zi(px, py);
                        if zi <= 0.0 {
                            g
                        } else {
                            // The visible point, a unit towards the eye, and its leaf.
                            let z = 1.0 / zi - 1.0;
                            let (rx, ry) = (
                                (xx as f64 - f64::from(view.xcenter)) / f64::from(view.xscale),
                                (f64::from(view.ycenter) - sp.v as f64) / f64::from(view.yscale),
                            );
                            let p: [f32; 3] = [0, 1, 2].map(|c| {
                                (f64::from(view.origin[c])
                                    + z * (f64::from(view.forward[c])
                                        + rx * f64::from(view.right[c])
                                        + ry * f64::from(view.up[c]))) as f32
                            });
                            let leaf = point_in_leaf(bsp, p).unwrap_or(0);
                            mix(grey(luma(g)), palette_colour(palette, leaf as u64 + 7), 0.7)
                        }
                    }
                },
                XrayBase::PixelsOff => {
                    let stepped = matches!(s.paint, XrayPaint::Cached | XrayPaint::Liquid);
                    let off = stepped && x.drawn.get(i).is_some() && x.drawn.get(i) != x.exact.get(i);
                    if off { mix(g, tint.colour, tint.alpha) } else { mix(g, [0, 0, 0], tint.dim) }
                }
                XrayBase::Error => match s.grads {
                    Some(gr) if matches!(s.paint, XrayPaint::Cached | XrayPaint::PerPixel | XrayPaint::Liquid) => {
                        let e = affine_error(&gr, sp.u as usize, sp.count as usize, k, sp.v as usize, x.persp)
                            / texel_scale(s);
                        heat(g, e)
                    }
                    _ => grey(luma(g) * 0.3),
                },
                _ => g,
            };
        }
    }
    out
}

/// Whether the pixel at `(x, y)` lies on a line of the `cell`-texel grid
/// (mip 0 texels): the cell under it differs from its right or lower
/// neighbour's.
fn on_grid(g: &XrayGrads, x: f64, y: f64, cell: f64) -> bool {
    let c = |x: f64, y: f64| {
        let [s, t] = g.st(x, y);
        ((s / cell).floor(), (t / cell).floor())
    };
    let here = c(x, y);
    here != c(x + 1.0, y) || here != c(x, y + 1.0)
}

/// Whether pixel `k` of a span of `count` pixels is one where the span's
/// texel is found exactly — a perspective divide — when the walls step
/// `n` pixels between them (`D_DrawSpans8`/`16`): the span's first pixel,
/// every `n`th after it, and its last (the last segment's end); every pixel
/// at exact (`n` 1).
pub fn is_divide(k: usize, count: usize, n: usize) -> bool {
    n <= 1 || k % n == 0 || k + 1 == count
}

/// The texel distance (mip 0) between the exact texel at pixel `k` of a span
/// of `count` from column `u` in row `v` and the one the affine spans of
/// `persp` read there: exact at each segment's first pixel and at the next
/// one's (or the span's last pixel, for a short last segment), straight in
/// between, as `D_DrawSpans8`/`16` step.
pub fn affine_error(g: &XrayGrads, u: usize, count: usize, k: usize, v: usize, persp: PerspSpan) -> f64 {
    let n = persp.pixels().max(1) as usize;
    if n == 1 {
        return 0.0;
    }
    let y = v as f64 + 0.5;
    let x = |k: usize| (u + k) as f64 + 0.5;
    let k0 = k / n * n;
    let k1 = (k0 + n).min(count - 1);
    let exact = g.st(x(k), y);
    if k1 <= k0 {
        return 0.0;
    }
    let (a, b) = (g.st(x(k0), y), g.st(x(k1), y));
    let f = (k - k0) as f64 / (k1 - k0) as f64;
    let affine = [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f];
    (affine[0] - exact[0]).hypot(affine[1] - exact[1])
}

/// `xray pixelsoff`'s look, in RGB: the pixels off exact `alpha` of the way
/// to `colour`, the rest `dim` of the way to black.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TintRgb {
    pub colour: [u8; 3],
    pub alpha: f32,
    pub dim: f32,
}

/// `divides`' marks: `colour`, `width` output pixels wide, `alpha` opaque.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Marks {
    pub colour: [u8; 3],
    pub width: f32,
    pub alpha: f32,
}

/// A thin mark at each perspective divide of the walls and liquids (the
/// pixels whose texel the span finds exactly, [`is_divide`]) over the output
/// frame `out`, as the perspective explainer page marks them: a bar `width`
/// output pixels wide down the middle of each such pixel, whatever the
/// mode's pixel size; at exact, where every pixel divides, each pixel whole.
/// Not where an entity covers the wall.
pub fn divides_over(out: &mut [u8], (ow, oh): (usize, usize), place: Place, x: &XrayFrame, marks: &Marks) {
    if marks.alpha <= 0.0 {
        return;
    }
    let n = x.persp.pixels().max(1) as usize;
    let half = marks.width / 2.0;
    let mut cover = |col: usize, row: usize, a: f32| {
        if col < ow && row < oh && a > 0.0 {
            let at = (row * ow + col) * 3;
            let c = mix([out[at], out[at + 1], out[at + 2]], marks.colour, a.min(1.0) * marks.alpha);
            out[at..at + 3].copy_from_slice(&c);
        }
    };
    for sp in &x.spans {
        let Some(Some(s)) = x.surfaces.get(sp.surface as usize) else { continue };
        if !matches!(s.paint, XrayPaint::Cached | XrayPaint::Liquid) {
            continue;
        }
        let v = sp.v as usize;
        let rows = |y: f32| (y.round().max(0.0) as usize).min(oh);
        let (r0, r1) = (
            rows(place.y0 + (v as f32 + place.vy) * place.ky),
            rows(place.y0 + (v as f32 + 1.0 + place.vy) * place.ky),
        );
        for k in 0..sp.count as usize {
            let u = sp.u as usize + k;
            let i = v * x.w + u;
            if !is_divide(k, sp.count as usize, n) || x.zbuf.get(i) != x.world_z.get(i) {
                continue;
            }
            let left = place.x0 + (u as f32 + place.vx) * place.kx;
            let (a, b) = if n <= 1 {
                (left, left + place.kx)
            } else {
                let mid = left + place.kx / 2.0;
                (mid - half, mid + half)
            };
            for col in (a.floor().max(0.0) as usize)..(b.ceil().max(0.0) as usize).min(ow) {
                // How much of the output column the bar covers.
                let c = (b.min(col as f32 + 1.0) - a.max(col as f32)).clamp(0.0, 1.0);
                for row in r0..r1 {
                    cover(col, row, c);
                }
            }
        }
    }
}

/// A heat colour for an error of `e` texels over the picture's grey.
fn heat(g: [u8; 3], e: f64) -> [u8; 3] {
    let dark = grey(luma(g) * 0.35);
    let e = e as f32;
    if e < 0.5 {
        mix(dark, [240, 200, 40], e / 0.5)
    } else {
        mix([240, 200, 40], [230, 40, 30], ((e - 0.5) / 0.5).min(1.0))
    }
}

/// Every world face drawn this frame, and every face in a leaf the PVS marked.
fn world_faces(x: &XrayFrame, bsp: &Bsp) -> (Vec<bool>, Vec<bool>) {
    let mut drawn = vec![false; bsp.faces.len()];
    for s in x.surfaces.iter().flatten() {
        if s.model == XrayModel::World {
            if let Some(d) = drawn.get_mut(s.face) {
                *d = true;
            }
        }
    }
    let mut in_pvs = vec![false; bsp.faces.len()];
    for (leaf, l) in bsp.leafs.iter().enumerate().skip(1) {
        if !x.leaf_visible.get(leaf).copied().unwrap_or(false) {
            continue;
        }
        let first = usize::from(l.firstmarksurface);
        for &f in bsp.marksurfaces.get(first..first + usize::from(l.nummarksurfaces)).unwrap_or(&[]) {
            if let Some(p) = in_pvs.get_mut(usize::from(f)) {
                *p = true;
            }
        }
    }
    (drawn, in_pvs)
}

/// Draw the wireframe: the world's edges in lava orange (with
/// `pvs`, light brown for a face in the PVS but not drawn, light slate for
/// one outside it), the entities' in pale flame, the gun's in blue.
/// at the output frame's resolution, crisp whatever the mode's: `out` is the
/// `ow x oh` RGB output frame, `place` where the view lies on it, and
/// `strength` the x-ray's (`mix`).
#[allow(clippy::too_many_arguments)]
pub fn wire_over(
    out: &mut [u8],
    (ow, oh): (usize, usize),
    place: Place,
    x: &XrayFrame,
    bsp: &Bsp,
    palette: &[[u8; 3]; 256],
    wire: Wire,
    strength: f64,
) {
    let Some(view) = x.view else { return };
    let view = &view;
    if strength <= 0.0 || !(wire.world || wire.entities) {
        return;
    }
    let mut lines =
        Lines { out, ow, oh, place, vw: x.w, vh: x.h, zbuf: &x.zbuf, through: wire.through, strength: strength as f32 };
    let classes = wire.pvs || wire.culled;
    if wire.world {
        let (drawn, in_pvs) = if classes { world_faces(x, bsp) } else { (Vec::new(), Vec::new()) };
        // Each edge once, in the colour of the most-drawn face it bounds.
        let mut best: Vec<u8> = vec![0; bsp.edges.len()];
        let model = bsp.models.first();
        let faces = model.map_or(0..0, |m| {
            let first = m.firstface.max(0) as usize;
            first..(first + m.numfaces.max(0) as usize).min(bsp.faces.len())
        });
        for fi in faces {
            let face = &bsp.faces[fi];
            let rank = match (classes, drawn.get(fi), in_pvs.get(fi)) {
                (false, ..) | (_, Some(true), _) => 3,
                (_, _, Some(true)) => 2,
                _ => 1,
            };
            for k in 0..face.numedges.max(0) as usize {
                if let Some(&se) = bsp.surfedges.get(face.firstedge.max(0) as usize + k) {
                    if let Some(b) = best.get_mut(se.unsigned_abs() as usize) {
                        *b = (*b).max(rank);
                    }
                }
            }
        }
        for (ei, &rank) in best.iter().enumerate() {
            if rank == 0 || (rank == 1 && !wire.through) || (wire.culled && rank != 1) {
                continue;
            }
            let e = &bsp.edges[ei];
            let (Some(a), Some(b)) = (bsp.vertexes.get(e.v[0] as usize), bsp.vertexes.get(e.v[1] as usize)) else {
                continue;
            };
            let (colour, strength) = match rank {
                3 => (palette[LAVA], 1.0),
                2 => (palette[LIGHT_BROWN], 0.8),
                _ => (palette[LIGHT_SLATE], 0.8),
            };
            lines.world(view, a.point, b.point, colour, strength);
        }
    }
    if wire.entities {
        for e in &x.brush_edges {
            lines.world(view, e[0], e[1], palette[FLAME], 1.0);
        }
        for t in &x.triangles {
            let colour = if t.gun { palette[BLUE] } else { palette[FLAME] };
            for k in 0..3 {
                let (a, b) = (t.v[k], t.v[(k + 1) % 3]);
                lines.screen(a, b, colour, 0.9);
            }
        }
    }
}

/// Where the view's pixels land on the output frame: the view's pixel `(x,
/// y)` (centres on the integers) is the screen's `(x + vx, y + vy)`, and the
/// screen's pixel `(sx, sy)` covers the output's `x0 + sx * kx ..` and `y0 +
/// sy * ky ..` (the fit's nearest scaling).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub vx: f32,
    pub vy: f32,
    pub x0: f32,
    pub y0: f32,
    pub kx: f32,
    pub ky: f32,
}

impl Place {
    /// A view point (pixel centres on the integers) on the output.
    fn on_output(&self, x: f32, y: f32) -> (f32, f32) {
        (self.x0 + (x + self.vx + 0.5) * self.kx - 0.5, self.y0 + (y + self.vy + 0.5) * self.ky - 0.5)
    }

    /// The view's pixel under the output's pixel `(x, y)`, if any.
    fn view_pixel(&self, x: usize, y: usize, vw: usize, vh: usize) -> Option<usize> {
        let px = ((x as f32 - self.x0 + 0.5) / self.kx).floor() - self.vx;
        let py = ((y as f32 - self.y0 + 0.5) / self.ky).floor() - self.vy;
        (px >= 0.0 && py >= 0.0 && (px as usize) < vw && (py as usize) < vh).then(|| py as usize * vw + px as usize)
    }
}

/// Lines onto the output frame, tested against the view's z-buffer.
struct Lines<'a> {
    out: &'a mut [u8],
    ow: usize,
    oh: usize,
    place: Place,
    vw: usize,
    vh: usize,
    zbuf: &'a [i16],
    through: bool,
    strength: f32,
}

impl Lines<'_> {
    /// A world-space segment: clipped to the near plane, projected.
    fn world(&mut self, view: &XrayView, a: [f32; 3], b: [f32; 3], colour: [u8; 3], strength: f32) {
        const NEAR: f32 = 1.0;
        let (mut va, mut vb) = (view.to_view(a), view.to_view(b));
        if va[2] < NEAR && vb[2] < NEAR {
            return;
        }
        if va[2] < NEAR || vb[2] < NEAR {
            let t = (NEAR - va[2]) / (vb[2] - va[2]);
            let p = [0, 1, 2].map(|k| va[k] + (vb[k] - va[k]) * t);
            if va[2] < NEAR {
                va = p;
            } else {
                vb = p;
            }
        }
        self.screen(view.screen(va), view.screen(vb), colour, strength);
    }

    /// A screen-space segment `(x, y, 1/z)`, anti-aliased (Xiaolin Wu's),
    /// each pixel tested against the z-buffer unless `through`.
    fn screen(&mut self, a: [f32; 3], b: [f32; 3], colour: [u8; 3], strength: f32) {
        let ((ax, ay), (bx, by)) = (self.place.on_output(a[0], a[1]), self.place.on_output(b[0], b[1]));
        let (a, b) = ([ax, ay, a[2]], [bx, by, b[2]]);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.abs().max(dy.abs());
        if !len.is_finite() || len > 1e5 {
            return;
        }
        let steps = len.ceil().max(1.0) as usize;
        let steep = dy.abs() > dx.abs();
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let (x, y, zi) = (a[0] + dx * t, a[1] + dy * t, a[2] + (b[2] - a[2]) * t);
            // The two pixels across the line, by how near its centre is.
            let (main, cross) = if steep { (y, x) } else { (x, y) };
            let c0 = cross.floor();
            let f = cross - c0;
            for (c, cover) in [(c0, 1.0 - f), (c0 + 1.0, f)] {
                let (px, py) = if steep { (c, main.round()) } else { (main.round(), c) };
                self.plot(px, py, zi, colour, cover * strength);
            }
        }
    }

    fn plot(&mut self, x: f32, y: f32, zi: f32, colour: [u8; 3], cover: f32) {
        if x < 0.0 || y < 0.0 || cover <= 0.0 {
            return;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= self.ow || y >= self.oh {
            return;
        }
        let Some(v) = self.place.view_pixel(x, y, self.vw, self.vh) else { return };
        if !self.through {
            // Behind what the frame drew there: hidden. With slack for the
            // 16-bit buffer, and against the farthest of the pixel and its
            // neighbours, so an edge on the boundary of what it bounds is
            // not lost to a coarse mode's pixels either side of it.
            let (vx, vy) = (v % self.vw, v / self.vw);
            let mut zb = i16::MAX;
            for ny in vy.saturating_sub(1)..(vy + 2).min(self.vh) {
                for nx in vx.saturating_sub(1)..(vx + 2).min(self.vw) {
                    zb = zb.min(self.zbuf.get(ny * self.vw + nx).copied().unwrap_or(0));
                }
            }
            if zi * 1.03 + 1.5 < f32::from(zb) {
                return;
            }
        }
        let at = (y * self.ow + x) * 3;
        let c = mix([self.out[at], self.out[at + 1], self.out[at + 2]], colour, cover * self.strength);
        self.out[at..at + 3].copy_from_slice(&c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_affine_error_is_zero_at_each_divide_and_at_exact() {
        // A wall receding to the right: 1/z falls along the row.
        let g = XrayGrads { zi: [0.02, -0.00001, 0.0], sz: [0.5, 0.01, 0.0], tz: [0.0, 0.0, 0.01], st_eye: [0.0, 0.0] };
        for k in [0, 16, 32] {
            assert!(affine_error(&g, 100, 40, k, 50, PerspSpan::Spans16) < 1e-9, "pixel {k}");
        }
        assert!(affine_error(&g, 100, 40, 8, 50, PerspSpan::Spans16) > 1e-6, "mid-segment");
        assert!(
            affine_error(&g, 100, 40, 8, 50, PerspSpan::Spans16) > affine_error(&g, 100, 40, 4, 50, PerspSpan::Spans8),
            "shorter spans err less"
        );
        assert_eq!(affine_error(&g, 100, 40, 8, 50, PerspSpan::Exact), 0.0);
    }
}
