//! Marks: points of the world — a torch, a nail, a grunt — followed onto
//! the picture, frame by frame, for an edit to hang a callout on.
//!
//! A mark is seen as the frame was drawn: projected by the renderer's own
//! view ([`XrayView`], `R_ViewChanged`'s projection) and tested against the
//! frame's z-buffer, the 16-bit `1/z` every world span and every model wrote
//! ([`XrayFrame::zbuf`]). It is *visible* when it is in the view and what the
//! frame drew there is no nearer than the point less its radius: a point
//! inside a model (an entity's middle) or on a wall is seen, one behind a
//! wall, a door or a passing grunt is not. Its record, in `events.json`'s
//! `marks`:
//!
//! ```text
//! { "frame": 12, "t": 0.2, "point": [1360, 936, 314], "screen": [961.5, 540.5],
//!   "in_view": true, "visible": true, "dist": 64.2 }
//! ```
//!
//! `screen` is in the output frame's pixels (edges on the integers, as
//! `events.json`'s), anywhere in front of the camera, also off the picture;
//! `null` behind it, or when the entity is not there (not yet, or no more).

use quake_rs::render::xray::{XrayFrame, XrayView, ZBUF_SCALE};

use super::events::Obj;
use super::xray::Place;

/// A mark as one frame shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Seen {
    /// The world point (`None`: its entity is not there).
    pub point: Option<[f64; 3]>,
    /// On the output frame, in front of the camera.
    pub screen: Option<[f64; 2]>,
    pub in_view: bool,
    pub visible: bool,
    /// From the eye, in units.
    pub dist: Option<f64>,
}

impl Seen {
    /// The record for film frame `frame` at film second `t`, its screen
    /// moved by `shift` (an `ab` side's place in the composed frame).
    pub fn json(&self, frame: usize, t: f64, shift: (f64, f64)) -> String {
        let point = self.point.map(|p| p.map(|v| v as f32));
        let screen = self.screen.map(|[x, y]| [(x + shift.0) as f32, (y + shift.1) as f32]);
        let mut o = Obj::kind("mark").int("frame", frame as i64).num("t", t);
        o = o.opt_vec("point", point.as_ref().map(|p| &p[..])).opt_vec("screen", screen.as_ref().map(|p| &p[..]));
        o = o.bool("in_view", self.in_view).bool("visible", self.visible);
        match self.dist {
            Some(d) => o.num("dist", d),
            None => o.null("dist"),
        }
        .end()
    }
}

/// `point` as the frame `x` was drawn, on the output frame where `place`
/// puts its view; `radius` units of slack behind what was drawn.
pub fn see(x: &XrayFrame, place: Place, point: [f64; 3], radius: f64) -> Seen {
    let mut seen = Seen { point: Some(point), ..Seen::default() };
    let Some(view) = x.view else { return seen };
    seen.dist = Some((0..3).map(|k| (point[k] - f64::from(view.origin[k])).powi(2)).sum::<f64>().sqrt());
    let Some((vx, vy, depth)) = on_view(&view, point) else { return seen };
    // On the output: the view's pixel `u` covers `x0 + (u + vx) * kx ..` (the fit).
    let out = |u: f64, o: f32, off: f32, k: f32| f64::from(o) + (u + 0.5 + f64::from(off)) * f64::from(k);
    seen.screen = Some([out(vx, place.x0, place.vx, place.kx), out(vy, place.y0, place.vy, place.ky)]);
    let (w, h) = (x.w as f64, x.h as f64);
    seen.in_view = vx >= -0.5 && vx < w - 0.5 && vy >= -0.5 && vy < h - 0.5;
    if seen.in_view {
        seen.visible = !occluded(&x.zbuf, (x.w, x.h), (vx, vy), depth, radius);
    }
    seen
}

/// A world point on the view: `(x, y)` with pixel centres on the integers,
/// and its depth; `None` behind the eye (nearer than a unit).
pub fn on_view(view: &XrayView, p: [f64; 3]) -> Option<(f64, f64, f64)> {
    let d = [0, 1, 2].map(|k| p[k] - f64::from(view.origin[k]));
    let dot = |v: [f32; 3]| d[0] * f64::from(v[0]) + d[1] * f64::from(v[1]) + d[2] * f64::from(v[2]);
    let depth = dot(view.forward);
    if depth < 1.0 {
        return None;
    }
    let x = f64::from(view.xcenter) + f64::from(view.xscale) * dot(view.right) / depth;
    let y = f64::from(view.ycenter) - f64::from(view.yscale) * dot(view.up) / depth;
    Some((x, y, depth))
}

/// Whether the frame drew something nearer than `depth - radius` at the
/// view point `(x, y)` — at it and at each pixel around it, so a point on a
/// silhouette's edge is seen if any of its neighbours shows it — with a unit
/// of the 16-bit buffer's slack.
pub fn occluded(zbuf: &[i16], (w, h): (usize, usize), (x, y): (f64, f64), depth: f64, radius: f64) -> bool {
    let (px, py) =
        ((x + 0.5).floor().clamp(0.0, w as f64 - 1.0) as usize, (y + 0.5).floor().clamp(0.0, h as f64 - 1.0) as usize);
    // The farthest drawn around it (the smallest 1/z).
    let mut far = i16::MAX;
    for ny in py.saturating_sub(1)..(py + 2).min(h) {
        for nx in px.saturating_sub(1)..(px + 2).min(w) {
            far = far.min(zbuf.get(ny * w + nx).copied().unwrap_or(0));
        }
    }
    let near = depth - radius;
    near > 1.0 && f64::from(far) > f64::from(ZBUF_SCALE) / near + 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 9x9 view looking along +x from the origin, 90 degrees wide.
    fn view() -> XrayView {
        XrayView {
            origin: [0.0; 3],
            forward: [1.0, 0.0, 0.0],
            right: [0.0, -1.0, 0.0],
            up: [0.0, 0.0, 1.0],
            xcenter: 4.0,
            ycenter: 4.0,
            xscale: 4.5,
            yscale: 4.5,
        }
    }

    #[test]
    fn a_point_lands_where_the_view_projects_it() {
        let v = view();
        assert_eq!(on_view(&v, [100.0, 0.0, 0.0]), Some((4.0, 4.0, 100.0)), "straight ahead: the centre");
        let (x, y, _) = on_view(&v, [100.0, -50.0, 50.0]).unwrap();
        assert!((x - 6.25).abs() < 1e-9 && (y - 1.75).abs() < 1e-9, "right and up: {x} {y}");
        assert_eq!(on_view(&v, [-10.0, 0.0, 0.0]), None, "behind");
        // On a frame 4x the view's size, with the view at (1, 2) of the screen.
        let mut x = XrayFrame { w: 9, h: 9, view: Some(v), zbuf: vec![0; 81], ..XrayFrame::default() };
        let place = Place { vx: 1.0, vy: 2.0, x0: 10.0, y0: 0.0, kx: 4.0, ky: 4.0 };
        let s = see(&x, place, [100.0, 0.0, 0.0], 8.0);
        assert_eq!(s.screen, Some([10.0 + 5.5 * 4.0, 6.5 * 4.0]), "the centre pixel's centre");
        assert!(s.in_view && s.visible && s.dist == Some(100.0));
        // Past the view's side: on the frame's plane, not in the view.
        let s = see(&x, place, [10.0, -20.0, 0.0], 8.0);
        assert!(s.screen.is_some() && !s.in_view && !s.visible);
        // A wall at 50 units in front of it hides it; a wall behind it does not.
        x.zbuf = vec![(ZBUF_SCALE / 50.0) as i16; 81];
        assert!(!see(&x, place, [100.0, 0.0, 0.0], 8.0).visible, "behind the wall");
        assert!(see(&x, place, [55.0, 0.0, 0.0], 8.0).visible, "within its radius of the wall");
        assert!(see(&x, place, [30.0, 0.0, 0.0], 8.0).visible, "in front of it");
    }
}
