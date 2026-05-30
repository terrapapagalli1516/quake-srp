//! A from-scratch software rasteriser driven by the parsed BSP data.
//!
//! This is **not** a port of Quake's asm-heavy `d_*.c` span renderer (the
//! original `WinQuake` software pipeline walked the BSP front-to-back, built
//! edge tables, and emitted affine-textured spans via hand-tuned x86). Instead
//! this module is a small, self-contained, allocation-light triangle
//! rasteriser written purely against the *parsed* [`Bsp`] lump records produced
//! by [`crate::bsp`]: it reconstructs each face polygon from the
//! surfedge/edge/vertex tables, transforms it into camera space, perspective
//! projects it, fan-triangulates, and fills triangles with barycentric
//! coverage plus a per-pixel depth buffer. Shading is a flat per-face Lambert
//! term over a stable per-surface hue — no palette, no lightmaps, no textures.
//!
//! Design goals, in priority order:
//!  * **Memory safety.** No `unsafe` (the crate is `#![forbid(unsafe_code)]`),
//!    no external crates — only `std`.
//!  * **Never panic on BSP-derived data.** Every index into a lump goes through
//!    `.get()`; a face referencing an out-of-range edge, surfedge, vertex,
//!    plane, or texinfo is silently skipped rather than indexed blindly. The
//!    only direct indexing is into our own freshly-allocated framebuffers,
//!    where the index is provably in bounds.
//!  * **Determinism.** Given the same `Bsp` and `Camera`, the output `Image` is
//!    bit-for-bit reproducible (no floating-point nondeterminism beyond IEEE).
//!
//! Coordinate conventions (Quake world space): `+X` east, `+Y` north, `+Z` up.
//! The camera looks down its own `+forward`; see [`render_bsp`] for the full
//! view transform and projection.

use crate::bsp::Bsp;
use crate::math::{cross, dot, normalize, sub, Vec3};

// ---------------------------------------------------------------------------
// Image
// ---------------------------------------------------------------------------

/// A simple row-major RGB framebuffer. `rgb[y * w + x]` is the pixel at
/// `(x, y)` with the origin at the top-left.
pub struct Image {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<[u8; 3]>,
}

impl Image {
    /// Allocate a `w * h` image filled with the background colour `bg`.
    pub fn new(w: usize, h: usize, bg: [u8; 3]) -> Image {
        // `w * h` could in principle overflow `usize` on absurd inputs; saturate
        // so we never wrap to a tiny allocation and then index past it.
        let count = w.saturating_mul(h);
        Image {
            w,
            h,
            rgb: vec![bg; count],
        }
    }

    /// Set the pixel at `(x, y)` to `c`. A bounds-checked no-op when the
    /// coordinate lies off-screen (including negative coordinates).
    pub fn put(&mut self, x: i32, y: i32, c: [u8; 3]) {
        if x < 0 || y < 0 {
            return;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= self.w || y >= self.h {
            return;
        }
        let idx = y * self.w + x;
        if let Some(p) = self.rgb.get_mut(idx) {
            *p = c;
        }
    }

    /// Write the image as a binary (P6) PPM file.
    pub fn write_ppm(&self, path: &str) -> std::io::Result<()> {
        use std::io::Write;
        let file = std::fs::File::create(path)?;
        let mut out = std::io::BufWriter::new(file);
        // P6 header: magic, width, height, maxval.
        out.write_all(format!("P6\n{} {}\n255\n", self.w, self.h).as_bytes())?;
        // Pixel payload: 3 bytes per pixel, row-major. Build one flat buffer so
        // we issue a single bulk write rather than a syscall per pixel.
        let mut raw = Vec::with_capacity(self.rgb.len().saturating_mul(3));
        for px in &self.rgb {
            raw.push(px[0]);
            raw.push(px[1]);
            raw.push(px[2]);
        }
        out.write_all(&raw)?;
        out.flush()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Camera
// ---------------------------------------------------------------------------

/// Quake's `V_CalcBob` (view.c): the sinusoidal head-bob amount (world units) to
/// add to the eye height while moving, so the view rocks up and down with each
/// step. `vel_xy` is the player's horizontal speed (units/sec) and `time` the
/// game clock (seconds). Uses the stock cvar defaults `cl_bob = 0.02`,
/// `cl_bobcycle = 0.6`, `cl_bobup = 0.5`; the result is clamped to `[-7, 4]`
/// exactly as the C. At rest (`vel_xy == 0`) the bob is 0, so a standing view is
/// unchanged. The first-person weapon, being anchored to the camera, stays
/// screen-stable while the world bobs — the classic Quake look.
pub fn view_bob(vel_xy: f32, time: f32) -> f32 {
    use std::f32::consts::PI;
    const CL_BOB: f32 = 0.02;
    const CL_BOBCYCLE: f32 = 0.6;
    const CL_BOBUP: f32 = 0.5;
    if !time.is_finite() || !vel_xy.is_finite() {
        return 0.0;
    }
    // Phase within the bob cycle, in [0, 1).
    let mut cycle = (time - (time / CL_BOBCYCLE).floor() * CL_BOBCYCLE) / CL_BOBCYCLE;
    cycle = if cycle < CL_BOBUP {
        PI * cycle / CL_BOBUP
    } else {
        PI + PI * (cycle - CL_BOBUP) / (1.0 - CL_BOBUP)
    };
    // Bob is proportional to horizontal speed, mostly the sin term.
    let base = vel_xy * CL_BOB;
    let bob = base * 0.3 + base * 0.7 * cycle.sin();
    bob.clamp(-7.0, 4.0)
}

/// The full-screen colour shift for a leaf content type (Quake's `cshift_water`
/// / `cshift_slime` / `cshift_lava` from view.c), as `(rgb, percent)` where
/// `percent` is 0..150. Empty / solid / sky return `None` (no tint).
pub fn content_cshift(contents: i32) -> Option<([u8; 3], f32)> {
    match contents {
        crate::bsp::CONTENTS_WATER => Some(([130, 80, 50], 128.0)),
        crate::bsp::CONTENTS_SLIME => Some(([0, 25, 5], 150.0)),
        crate::bsp::CONTENTS_LAVA => Some(([255, 80, 0], 150.0)),
        _ => None,
    }
}

/// Combine colour shifts `(rgb, percent 0..255)` into a single blend colour and
/// alpha (0..1), porting Quake's `V_CalcBlend` accumulation (each shift is
/// alpha-over the running total). Empty list / all-zero percents give alpha 0.
pub fn combine_cshifts(shifts: &[([u8; 3], f32)]) -> ([u8; 3], f32) {
    let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for &(color, percent) in shifts {
        if !(percent > 0.0) {
            continue;
        }
        let a2 = (percent / 255.0).clamp(0.0, 1.0);
        a += a2 * (1.0 - a);
        if a <= 0.0 {
            continue;
        }
        let an = (a2 / a).clamp(0.0, 1.0); // share of the new colour in the mix
        r = r * (1.0 - an) + color[0] as f32 * an;
        g = g * (1.0 - an) + color[1] as f32 * an;
        b = b * (1.0 - an) + color[2] as f32 * an;
    }
    let to_u8 = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    ([to_u8(r), to_u8(g), to_u8(b)], a.clamp(0.0, 1.0))
}

/// Blend `color` over every pixel of `image` at `alpha` (0..1) — the full-screen
/// polyblend (damage flash, underwater/lava/slime tint). `alpha <= 0` is a no-op.
/// Apply this to the 3-D frame *before* the status-bar HUD (Quake never tints
/// the sbar).
pub fn apply_blend(image: &mut Image, color: [u8; 3], alpha: f32) {
    if !(alpha > 0.0) {
        return;
    }
    let a = alpha.min(1.0);
    let inv = 1.0 - a;
    for px in image.rgb.iter_mut() {
        for c in 0..3 {
            px[c] = (px[c] as f32 * inv + color[c] as f32 * a).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// A pinhole camera positioned in Quake world space. `yaw` rotates about `+Z`
/// (0 = facing `+X`, increasing toward `+Y`); `pitch` tilts the forward vector
/// up/down. Both are in degrees, as is the horizontal field of view `fov_deg`.
pub struct Camera {
    pub pos: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub fov_deg: f32,
}

impl Camera {
    /// Build a camera at `pos` aimed at `target`, deriving `yaw`/`pitch` from
    /// the direction between them. A degenerate (zero-length) look vector falls
    /// back to facing `+X` with no pitch.
    pub fn looking_at(pos: [f32; 3], target: [f32; 3], fov_deg: f32) -> Camera {
        let dir = sub(target, pos);
        let horiz = ((dir[0] as f64) * (dir[0] as f64) + (dir[1] as f64) * (dir[1] as f64)).sqrt();

        // yaw: angle of the XY projection, measured from +X toward +Y.
        let yaw = if horiz == 0.0 && dir[1] == 0.0 && dir[0] == 0.0 {
            0.0
        } else {
            (dir[1] as f64).atan2(dir[0] as f64).to_degrees() as f32
        };

        // pitch: elevation above the XY plane. atan2(dz, horizontal_distance).
        let pitch = if horiz == 0.0 && dir[2] == 0.0 {
            0.0
        } else {
            (dir[2] as f64).atan2(horiz).to_degrees() as f32
        };

        Camera {
            pos,
            yaw,
            pitch,
            fov_deg,
        }
    }

    /// The orthonormal camera basis `(forward, right, up)` in world space.
    ///
    /// `forward` is the view direction (the camera looks down `+forward`).
    /// `right` points to the camera's right, `up` to its top. Derived directly
    /// from `yaw`/`pitch` so the renderer's view transform is fully under our
    /// control (rather than depending on Quake's `AngleVectors` sign quirks).
    fn basis(&self) -> (Vec3, Vec3, Vec3) {
        let cy = (self.yaw as f64).to_radians();
        let cp = (self.pitch as f64).to_radians();
        let (sin_y, cos_y) = (cy.sin(), cy.cos());
        let (sin_p, cos_p) = (cp.sin(), cp.cos());

        // forward: yaw rotates in XY, pitch lifts in Z.
        let forward: Vec3 = [
            (cos_p * cos_y) as f32,
            (cos_p * sin_y) as f32,
            sin_p as f32,
        ];
        // right: forward rotated -90 deg about +Z, kept level (no pitch), so the
        // horizon stays horizontal regardless of pitch. (cos_y, sin_y) -> rotate
        // by -90 -> (sin_y, -cos_y).
        let right: Vec3 = [sin_y as f32, -cos_y as f32, 0.0];
        // up = right x forward completes a right-handed (right, up, forward) set.
        let up = cross(right, forward);
        (forward, right, up)
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// A vertex after projection: integer-ish screen position kept as `f32` for
/// sub-pixel barycentric coverage, plus the camera-space forward depth used by
/// the z-buffer.
#[derive(Clone, Copy)]
struct Projected {
    x: f32,
    y: f32,
    depth: f32,
}

/// Hash a (non-negative) surface index to a stable, reasonably saturated RGB
/// base colour, so distinct textures/surfaces get distinct hues across runs.
fn hash_color(index: i64) -> [f32; 3] {
    // A small integer hash (splitmix-ish) to spread adjacent indices apart.
    let mut h = (index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 32;

    // Derive a hue in [0,1); keep saturation/value high but not blinding.
    let hue = ((h & 0xFFFF) as f32) / 65536.0;
    hsv_to_rgb(hue, 0.55, 0.95)
}

/// Convert HSV (each in `[0,1]`) to linear-ish RGB in `[0,1]`.
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h6 = (h.fract().abs() * 6.0).min(5.999_999);
    let i = h6 as i32;
    let f = h6 - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match i {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// Edge function: signed area (times two) of the triangle `(a, b, c)`. Positive
/// when `c` is to the left of the directed edge `a -> b` in screen space.
#[inline]
fn edge(ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32) -> f32 {
    (bx - ax) * (cy - ay) - (by - ay) * (cx - ax)
}

/// Rasterise one projected triangle into `image`/`zbuf` with barycentric
/// coverage and per-pixel depth interpolation. `color` is the already-shaded
/// 8-bit RGB for the whole triangle (flat shading).
fn raster_triangle(
    image: &mut Image,
    zbuf: &mut [f32],
    v0: Projected,
    v1: Projected,
    v2: Projected,
    color: [u8; 3],
) {
    let w = image.w;
    let h = image.h;
    if w == 0 || h == 0 {
        return;
    }

    // Screen-space bounding box, clamped to the framebuffer.
    let min_xf = v0.x.min(v1.x).min(v2.x);
    let max_xf = v0.x.max(v1.x).max(v2.x);
    let min_yf = v0.y.min(v1.y).min(v2.y);
    let max_yf = v0.y.max(v1.y).max(v2.y);

    // Reject triangles entirely off-screen or with non-finite coordinates.
    if !(min_xf.is_finite() && max_xf.is_finite() && min_yf.is_finite() && max_yf.is_finite()) {
        return;
    }

    let min_x = min_xf.floor().max(0.0) as i64;
    let max_x = max_xf.ceil().min((w as i64 - 1) as f32) as i64;
    let min_y = min_yf.floor().max(0.0) as i64;
    let max_y = max_yf.ceil().min((h as i64 - 1) as f32) as i64;
    if min_x > max_x || min_y > max_y {
        return;
    }

    // Twice the signed triangle area; if ~0 the triangle is degenerate.
    let area = edge(v0.x, v0.y, v1.x, v1.y, v2.x, v2.y);
    if area.abs() < 1e-6 {
        return;
    }
    let inv_area = 1.0 / area;

    for py in min_y..=max_y {
        for px in min_x..=max_x {
            // Sample at pixel centres.
            let sx = px as f32 + 0.5;
            let sy = py as f32 + 0.5;

            // Barycentric weights via edge functions.
            let w0 = edge(v1.x, v1.y, v2.x, v2.y, sx, sy) * inv_area;
            let w1 = edge(v2.x, v2.y, v0.x, v0.y, sx, sy) * inv_area;
            let w2 = edge(v0.x, v0.y, v1.x, v1.y, sx, sy) * inv_area;

            // Inside test: all weights non-negative (covers both windings since
            // inv_area carries the sign).
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }

            // Interpolate camera-space depth (linear in screen space is an
            // approximation, but adequate for hidden-surface ordering here).
            let depth = w0 * v0.depth + w1 * v1.depth + w2 * v2.depth;

            // Indices are provably in range: px in [0, w-1], py in [0, h-1].
            let idx = (py as usize) * w + (px as usize);
            if let Some(z) = zbuf.get_mut(idx) {
                if depth < *z {
                    *z = depth;
                    if let Some(p) = image.rgb.get_mut(idx) {
                        *p = color;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The renderer
// ---------------------------------------------------------------------------

/// Render every face of `bsp` from the viewpoint of `cam` into a `w * h` image.
///
/// ## View transform
/// World points are translated by `-cam.pos`, then projected onto the camera
/// basis `(right, up, forward)` to obtain camera space `(vx, vy, vz)`:
/// `vx = dot(rel, right)`, `vy = dot(rel, up)`, `vz = dot(rel, forward)`.
/// `vz` is the forward depth (positive = in front of the camera).
///
/// ## Projection
/// Focal length `f = (w/2) / tan(fov/2)`. Screen coordinates are
/// `x_screen = cx + f * vx / vz`, `y_screen = cy - f * vy / vz` (note the `-`
/// so `+up` maps to the top of the image). Faces with any vertex at or behind
/// the near plane (`vz <= NEAR`) are skipped (a simple, demo-grade near clip).
///
/// ## Culling
/// The face normal is `planes[planenum].normal`, negated when `face.side != 0`.
/// A face is back-facing — and skipped — when its outward normal points away
/// from the camera, i.e. `dot(normal, face_center - cam.pos) >= 0`. For the
/// inward-facing walls of [`demo_room`] this leaves exactly the walls the
/// interior camera should see.
///
/// ## Shading
/// Flat per face: a stable hue hashed from the `texinfo.miptex` index (or the
/// texinfo index when miptex is unavailable), modulated by a Lambert term
/// `max(0.15, dot(normal, light_dir))` with `light_dir` normalised from
/// `(0.3, 0.5, 1.0)`.
pub fn render_bsp(bsp: &Bsp, cam: &Camera, w: usize, h: usize) -> Image {
    const NEAR: f32 = 1.0;
    let bg: [u8; 3] = [12, 12, 18];
    let mut image = Image::new(w, h, bg);

    if w == 0 || h == 0 {
        return image;
    }

    let mut zbuf = vec![f32::INFINITY; w.saturating_mul(h)];

    let (forward, right, up) = cam.basis();
    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;
    // Focal length from horizontal fov; guard against degenerate fov values.
    let half_fov = (cam.fov_deg as f64 * 0.5).to_radians();
    let tan_half = half_fov.tan();
    let focal = if tan_half.abs() < 1e-6 {
        cx // fall back to ~90 deg-ish behaviour
    } else {
        (cx as f64 / tan_half) as f32
    };

    // Light direction, normalised once.
    let (light_dir, _len) = normalize([0.3, 0.5, 1.0]);

    // Scratch reused per face to avoid per-face allocation churn.
    let mut world_poly: Vec<Vec3> = Vec::new();
    let mut proj_poly: Vec<Projected> = Vec::new();

    for face in &bsp.faces {
        let numedges = face.numedges as i64;
        if numedges < 3 {
            // Need at least a triangle.
            continue;
        }
        let firstedge = face.firstedge as i64;
        if firstedge < 0 {
            continue;
        }

        // --- Reconstruct the polygon's world-space vertices in order. ---
        world_poly.clear();
        let mut bad = false;
        for i in 0..numedges {
            let se_index: usize = match (firstedge + i).try_into() {
                Ok(idx) => idx,
                Err(_) => {
                    bad = true;
                    break;
                }
            };
            let se = match bsp.surfedges.get(se_index) {
                Some(&s) => s,
                None => {
                    bad = true;
                    break;
                }
            };

            // se >= 0 -> edge[se], use v[0]; se < 0 -> edge[-se], use v[1].
            let (edge_index, vtx_slot): (usize, usize) = if se >= 0 {
                (se as usize, 0)
            } else {
                // -se as a usize; guard the i32::MIN edge case.
                match (se as i64).checked_neg() {
                    Some(n) if n >= 0 => (n as usize, 1),
                    _ => {
                        bad = true;
                        break;
                    }
                }
            };

            let edge = match bsp.edges.get(edge_index) {
                Some(e) => e,
                None => {
                    bad = true;
                    break;
                }
            };
            let vtx_id = match edge.v.get(vtx_slot) {
                Some(&id) => id as usize,
                None => {
                    bad = true;
                    break;
                }
            };
            let vertex = match bsp.vertexes.get(vtx_id) {
                Some(v) => v.point,
                None => {
                    bad = true;
                    break;
                }
            };
            world_poly.push(vertex);
        }
        if bad || world_poly.len() < 3 {
            continue;
        }

        // --- Face normal from the plane, flipped for back-side faces. ---
        let plane_index = face.planenum as i64;
        if plane_index < 0 {
            continue;
        }
        let plane = match plane_index
            .try_into()
            .ok()
            .and_then(|pi: usize| bsp.planes.get(pi))
        {
            Some(p) => p,
            None => continue,
        };
        let mut normal = plane.normal;
        if face.side != 0 {
            normal = [-normal[0], -normal[1], -normal[2]];
        }

        // Face center (average of the polygon vertices) for the cull test.
        let mut center = [0.0f32, 0.0, 0.0];
        for v in &world_poly {
            center[0] += v[0];
            center[1] += v[1];
            center[2] += v[2];
        }
        let inv_n = 1.0 / world_poly.len() as f32;
        center = [center[0] * inv_n, center[1] * inv_n, center[2] * inv_n];

        // Backface cull: outward normals facing away from the camera are hidden.
        // For demo_room's inward walls this keeps exactly the interior-facing
        // surfaces. (>= 0 => facing away.)
        let to_face = sub(center, cam.pos);
        if dot(normal, to_face) >= 0.0 {
            continue;
        }

        // --- Transform to camera space and project. ---
        proj_poly.clear();
        let mut clipped = false;
        for v in &world_poly {
            let rel = sub(*v, cam.pos);
            let vz = dot(rel, forward);
            if vz <= NEAR {
                // Near-plane reject for the whole face (demo-grade simple skip).
                clipped = true;
                break;
            }
            let vx = dot(rel, right);
            let vy = dot(rel, up);
            let sx = cx + focal * vx / vz;
            let sy = cy - focal * vy / vz;
            proj_poly.push(Projected {
                x: sx,
                y: sy,
                depth: vz,
            });
        }
        if clipped || proj_poly.len() < 3 {
            continue;
        }

        // --- Flat Lambert shade. ---
        let base = {
            // Prefer the miptex index for the hue; fall back to texinfo index.
            let texinfo_index = face.texinfo as i64;
            let surf_key = if texinfo_index >= 0 {
                match texinfo_index
                    .try_into()
                    .ok()
                    .and_then(|ti: usize| bsp.texinfo.get(ti))
                {
                    Some(ti) => ti.miptex as i64,
                    None => texinfo_index,
                }
            } else {
                texinfo_index
            };
            hash_color(surf_key)
        };

        let lambert = dot(normal, light_dir).max(0.15);
        let shade = lambert.min(1.0);
        let color = [
            (base[0] * shade * 255.0).clamp(0.0, 255.0) as u8,
            (base[1] * shade * 255.0).clamp(0.0, 255.0) as u8,
            (base[2] * shade * 255.0).clamp(0.0, 255.0) as u8,
        ];

        // --- Fan-triangulate (v0, vi, vi+1) and rasterise. ---
        let v0 = proj_poly[0];
        for i in 1..proj_poly.len() - 1 {
            // Indices i and i+1 are < len, in range by the loop bound.
            let v1 = proj_poly[i];
            let v2 = proj_poly[i + 1];
            raster_triangle(&mut image, &mut zbuf, v0, v1, v2, color);
        }
    }

    image
}

// ---------------------------------------------------------------------------
// Textured rendering (real Quake miptextures sampled through a palette)
// ---------------------------------------------------------------------------

/// Parse a Quake palette lump (`gfx/palette.lmp`): 256 RGB triples = 768 bytes.
pub fn parse_palette(bytes: &[u8]) -> Option<[[u8; 3]; 256]> {
    if bytes.len() < 768 {
        return None;
    }
    let mut pal = [[0u8; 3]; 256];
    for (i, px) in pal.iter_mut().enumerate() {
        let o = i * 3;
        *px = [bytes[o], bytes[o + 1], bytes[o + 2]];
    }
    Some(pal)
}

/// Reconstruct a face's world-space polygon into `out`. Returns false if any
/// index is out of range (the caller then skips the face). Mirrors the
/// surfedge/edge/vertex walk in [`render_bsp`].
fn face_world_poly(bsp: &Bsp, face: &crate::bsp::DFace, out: &mut Vec<Vec3>) -> bool {
    out.clear();
    let numedges = face.numedges as i64;
    if numedges < 3 {
        return false;
    }
    let firstedge = face.firstedge as i64;
    if firstedge < 0 {
        return false;
    }
    for i in 0..numedges {
        let se_index: usize = match (firstedge + i).try_into() {
            Ok(x) => x,
            Err(_) => return false,
        };
        let se = match bsp.surfedges.get(se_index) {
            Some(&s) => s,
            None => return false,
        };
        let (edge_index, slot): (usize, usize) = if se >= 0 {
            (se as usize, 0)
        } else {
            match (se as i64).checked_neg() {
                Some(n) if n >= 0 => (n as usize, 1),
                _ => return false,
            }
        };
        let edge = match bsp.edges.get(edge_index) {
            Some(e) => e,
            None => return false,
        };
        let vid = match edge.v.get(slot) {
            Some(&id) => id as usize,
            None => return false,
        };
        match bsp.vertexes.get(vid) {
            Some(v) => out.push(v.point),
            None => return false,
        }
    }
    out.len() >= 3
}

/// Outward face normal from its plane, flipped for back-side faces.
fn face_normal(bsp: &Bsp, face: &crate::bsp::DFace) -> Option<Vec3> {
    let pi: usize = (face.planenum as i64).try_into().ok()?;
    let p = bsp.planes.get(pi)?;
    let mut n = p.normal;
    if face.side != 0 {
        n = [-n[0], -n[1], -n[2]];
    }
    Some(n)
}

// ---------------------------------------------------------------------------
// BSP lightmaps (Quake's baked static lighting from the LIGHTING lump)
// ---------------------------------------------------------------------------

/// The luxel store behind a [`LightMap`]: either the face's static one-byte
/// luxels borrowed straight from `Bsp::lighting` (the common case, with no
/// dynamic light touching the face), or an owned `f32` grid that is the static
/// luxels plus the per-luxel dynamic-light contributions (`R_AddDynamicLights`).
///
/// Keeping a borrowed variant means a face with no dynamic light reads the exact
/// same bytes the pre-dlight code did, so its rendered output is byte-identical:
/// passing an empty dlight slice changes nothing.
enum Luxels<'a> {
    /// Borrowed static luxels, one byte each (`0..=255`).
    Static(&'a [u8]),
    /// Owned augmented luxels (`static + dynamic`), already in `0..=255`-luxel
    /// units but stored as `f32` so overbright (dynamic) values exceed 255.
    Owned(Vec<f32>),
}

impl Luxels<'_> {
    /// The luxel value at flat index `idx`, or `255.0` (fullbright) for any
    /// out-of-range index — so a malformed lightmap never reads garbage / panics.
    #[inline]
    fn at(&self, idx: usize) -> f32 {
        match self {
            Luxels::Static(s) => s.get(idx).copied().unwrap_or(255) as f32,
            Luxels::Owned(v) => v.get(idx).copied().unwrap_or(255.0),
        }
    }
}

/// A face's baked lightmap, borrowed from `Bsp::lighting` for the common case of
/// a single steady style-0 face, or an owned `f32` grid that is the multi-style
/// combine (`R_BuildLightMap`) plus any dynamic-light contributions.
///
/// `luxels` is a `lmw * lmh` grid (one value per luxel). `texmins` is the surface
/// texture-coordinate origin (in texels) used to convert a face's surface `(s,t)`
/// into luxel coordinates. The lightmap shares the face's `texinfo.vecs` with the
/// wall texture, so the same surface `(s,t)` the rasteriser already interpolates
/// indexes both.
struct LightMap<'a> {
    luxels: Luxels<'a>,
    lmw: usize,
    lmh: usize,
    texmins: [f32; 2],
}

/// Upper bound on the lightmap brightness factor. Quake's software renderer
/// clamped `blocklights` (the static+dynamic luxel sum, in 8.8 units) to a max
/// before the final shift; here we clamp the *factor* instead. The static path
/// tops out at `(255/255)*2 = 2.0`; dynamic lights add on top, so we allow up to
/// `4.0` (the rough WinQuake overbright ceiling of ~`4*255` luxels) and clamp
/// there, keeping a near light bright without letting a huge `(rad-dist)` blow
/// out to NaN/Inf or wrap a palette index.
const MAX_LIGHT_FACTOR: f32 = 4.0;

/// Number of animated light styles (`MAX_LIGHTSTYLES`), matching
/// [`crate::server::MAX_LIGHTSTYLES`]. The renderer takes a `[f32; LIGHTSTYLES]`
/// per-style brightness scale (1.0 == normal) so it can combine a face's
/// multiple lightmap layers (`R_BuildLightMap`).
pub const LIGHTSTYLES: usize = 64;

/// A no-op light-style scale table: every style at the "normal" `1.0`. Passing
/// this to [`render_scene_ext`] leaves lightmaps exactly as the static (style-0)
/// renderer produced them, which is what [`render_scene`] does — so all prior
/// behaviour and tests are unchanged. The animated front-ends instead pass
/// `server.lightstyle_scales(time)`.
pub const NEUTRAL_LIGHTSTYLE_SCALES: [f32; LIGHTSTYLES] = [1.0; LIGHTSTYLES];

/// `DFace.styles` slot value meaning "this lightmap layer is unused".
const STYLE_NONE: u8 = 255;

impl LightMap<'_> {
    /// Bilinearly sample the lightmap at surface texture coordinate `(s, t)`,
    /// returning a brightness factor (1.0 == neutral, up to ~2.0 overbright for
    /// the static lightmap, up to [`MAX_LIGHT_FACTOR`] with dynamic lights).
    ///
    /// Luxel indices are clamped into `[0, lm-1]`. Any index that somehow falls
    /// outside the luxel store is treated as the fullbright value 255, so a
    /// malformed lightmap never reads garbage and never panics.
    fn factor_at(&self, s: f32, t: f32) -> f32 {
        // lmw/lmh are >= 1 by construction in `face_lightmap`.
        let max_x = self.lmw.saturating_sub(1) as f32;
        let max_y = self.lmh.saturating_sub(1) as f32;
        let lxf = ((s - self.texmins[0]) / 16.0).clamp(0.0, max_x);
        let lyf = ((t - self.texmins[1]) / 16.0).clamp(0.0, max_y);

        let x0 = lxf.floor() as usize;
        let y0 = lyf.floor() as usize;
        let x1 = (x0 + 1).min(self.lmw.saturating_sub(1));
        let y1 = (y0 + 1).min(self.lmh.saturating_sub(1));
        let fx = lxf - x0 as f32;
        let fy = lyf - y0 as f32;

        let at = |x: usize, y: usize| -> f32 { self.luxels.at(y * self.lmw + x) };

        let top = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
        let bot = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
        let light = top * (1.0 - fy) + bot * fy;

        // The static byte path is byte-identical to before; the clamp only ever
        // bites when dynamic lights push a luxel above ~510.
        ((light / 255.0) * 2.0).min(MAX_LIGHT_FACTOR)
    }
}

/// `R_AddDynamicLights` (`r_surf.c`): fold the dynamic lights in `dlights` that
/// touch a face into an owned augmented luxel buffer.
///
/// `base` is the face's pre-combined luxel buffer when it already differs from
/// the plain static style-0 bytes — i.e. the multi-style combine from
/// [`build_styled_luxels`] (animated light styles). When `base` is `Some`, that
/// buffer is the starting point and any reaching dynamic light adds onto it, so
/// the result is `Some` even if no light reaches (the animated combine must still
/// be used). When `base` is `None`, the buffer is lazily materialised from
/// `static_samples` and the function returns `None` if no light reaches — so a
/// steady single-style face with an empty dlight slice keeps borrowing the static
/// bytes (byte-identical to before).
///
/// For each light: `dist = dot(origin, plane.normal) - plane.dist` (the RAW
/// plane, as the C uses `surf->plane->normal` directly — `dist.abs()` covers
/// both sides), `rad = radius - dist.abs()`. Skip if `rad < minlight`. Project
/// the light onto the surface plane (`impact = origin - normal*dist`), map it to
/// luxel space through the same `texinfo.vecs`/`texmins` the static lightmap
/// uses, and for each luxel add `rad - dist2` where
/// `dist2 = max(sd,td) + min(sd,td)/2` is Quake's cheap distance estimate.
///
/// The add is in `0..255` luxel units (the C `(rad-dist)*256` matches `luxel*256`
/// scaling; here both sides are kept in raw luxel units, so no `*256`). The
/// `factor_at` clamp bounds the result.
#[allow(clippy::too_many_arguments)]
fn add_dynamic_lights(
    bsp: &Bsp,
    face: &crate::bsp::DFace,
    ti: &crate::bsp::TexInfo,
    texmins: [f32; 2],
    lmw: usize,
    lmh: usize,
    static_samples: &[u8],
    base: Option<Vec<f32>>,
    dlights: &[crate::dlight::DynamicLight],
) -> Option<Vec<f32>> {
    // No dlights: the animated combine (if any) is the final buffer; otherwise
    // there is nothing to do and the caller keeps the static borrow.
    if dlights.is_empty() {
        return base;
    }
    // No usable plane: we can't project lights, but a pre-combined animated
    // buffer must still be returned so styles still animate.
    let plane = match (face.planenum as i64)
        .try_into()
        .ok()
        .and_then(|pi: usize| bsp.planes.get(pi))
    {
        Some(p) => p,
        None => return base,
    };
    let normal = plane.normal;

    // Start from the pre-combined animated buffer when present; otherwise the
    // buffer is lazily materialised from the static bytes on first contribution.
    let mut buf: Option<Vec<f32>> = base;

    for dl in dlights {
        let dist = dot(dl.origin, normal) - plane.dist;
        let rad = dl.radius - dist.abs();
        let minlight = dl.minlight;
        if rad < minlight {
            continue; // this light does not reach the face
        }
        let reach = rad - minlight; // C `minlight = rad - minlight`

        let impact = [
            dl.origin[0] - normal[0] * dist,
            dl.origin[1] - normal[1] * dist,
            dl.origin[2] - normal[2] * dist,
        ];

        // Project the impact point into luxel space via the texinfo axes (the
        // same vecs the static lightmap / surface extents use).
        let local0 = impact[0] * ti.vecs[0][0]
            + impact[1] * ti.vecs[0][1]
            + impact[2] * ti.vecs[0][2]
            + ti.vecs[0][3]
            - texmins[0];
        let local1 = impact[0] * ti.vecs[1][0]
            + impact[1] * ti.vecs[1][1]
            + impact[2] * ti.vecs[1][2]
            + ti.vecs[1][3]
            - texmins[1];

        // Lazily materialise the owned buffer from the static bytes the first
        // time a light actually contributes, so a non-reaching light slice still
        // returns None (borrow stays static, output unchanged).
        let buffer = buf.get_or_insert_with(|| {
            let mut v = vec![0.0f32; lmw * lmh];
            for (i, cell) in v.iter_mut().enumerate() {
                *cell = static_samples.get(i).copied().unwrap_or(0) as f32;
            }
            v
        });

        for t in 0..lmh {
            let td = (local1 - t as f32 * 16.0).abs();
            for s in 0..lmw {
                let sd = (local0 - s as f32 * 16.0).abs();
                let dist2 = if sd > td { sd + td * 0.5 } else { td + sd * 0.5 };
                if dist2 < reach {
                    let idx = t * lmw + s;
                    if let Some(cell) = buffer.get_mut(idx) {
                        *cell += rad - dist2;
                    }
                }
            }
        }
    }

    buf
}

/// `R_BuildLightMap` multi-style combine: read every active style block of a
/// face's lightmap and combine them into an owned `lmw*lmh` `f32` luxel buffer,
/// scaling each block by its style's brightness (`light_styles[style]`).
///
/// The LIGHTING lump stores one `lmw*lmh` luxel block PER ACTIVE style slot,
/// CONCATENATED in slot order at `face.lightofs`: the block for `styles[0]`
/// first, then `styles[1]`, etc. A slot value of `255` ([`STYLE_NONE`]) means the
/// layer is unused (no block stored). For each active slot `k` the combine adds
/// `block_k[i] * light_styles[styles[k]]` into luxel `i`.
///
/// Returns:
/// * `None` when the face has a single style-0 layer whose scale is exactly the
///   normal `1.0` — the common steady case. The caller then borrows the static
///   bytes directly so the rendered pixels are BYTE-IDENTICAL to the pre-style
///   renderer (`effective_luxel == block0`).
/// * `Some(buf)` for every other case (a non-neutral scale, or 2+ active styles).
///   `buf` is the combined `f32` grid, the base the dynamic-light step adds onto.
///
/// `light_styles` is the per-style brightness (`1.0` == normal). An out-of-range
/// style index reads as `1.0` (treated as normal), matching the server's
/// "missing style -> normal" rule so a face never goes dark referencing an unset
/// style.
///
/// Bounds: the whole `n_active * block` range (`block == lmw*lmh` luxels) is
/// checked against `lighting`. If it does not fit, returns [`StyleCombine::TooShort`]
/// so the caller falls back to the single-block (style-0) read or fullbright —
/// never reading out of bounds.
fn build_styled_luxels(
    face: &crate::bsp::DFace,
    lighting: &[u8],
    start: usize,
    block: usize,
    light_styles: &[f32; LIGHTSTYLES],
) -> StyleCombine {
    // Active style slots, in stored order. `255` marks an unused slot. The
    // LIGHTING lump stores exactly one block per *leading* active slot, so we
    // STOP at the first `255` (matching the C `R_BuildLightMap` loop). Reading
    // past a 255 would pull adjacent faces' luxels into a phantom block.
    let mut active: [(u8, f32); crate::bsp::MAXLIGHTMAPS] = [(STYLE_NONE, 1.0); crate::bsp::MAXLIGHTMAPS];
    let mut n_active = 0usize;
    for &style in face.styles.iter() {
        if style == STYLE_NONE {
            break;
        }
        let scale = light_styles.get(style as usize).copied().unwrap_or(1.0);
        active[n_active] = (style, scale);
        n_active += 1;
    }

    // No active style (lightofs >= 0 but styles all unused): nothing to combine;
    // let the caller keep the single static block (its existing behaviour).
    if n_active == 0 {
        return StyleCombine::StaticBlock;
    }

    // The single steady style-0 (or any single style) case at the normal scale is
    // byte-identical to reading the static block, so keep the borrow.
    if n_active == 1 && (active[0].1 - 1.0).abs() < f32::EPSILON {
        return StyleCombine::StaticBlock;
    }

    // The whole concatenated multi-block range must fit; otherwise fall back.
    let total = match block.checked_mul(n_active).and_then(|t| start.checked_add(t)) {
        Some(t) if t <= lighting.len() => t,
        _ => return StyleCombine::TooShort,
    };
    let all = &lighting[start..total];

    let mut buf = vec![0.0f32; block];
    for (k, &(_style, scale)) in active.iter().take(n_active).enumerate() {
        let off = k * block;
        // `off..off+block` is in range by the `total` check above.
        let blk = &all[off..off + block];
        for (i, cell) in buf.iter_mut().enumerate() {
            cell.add_assign_scaled(blk[i] as f32, scale);
        }
    }
    StyleCombine::Combined(buf)
}

/// Outcome of [`build_styled_luxels`].
enum StyleCombine {
    /// Keep the borrowed single static block (steady style-0 at normal scale).
    StaticBlock,
    /// Use this owned combined `f32` buffer as the base.
    Combined(Vec<f32>),
    /// The multi-block range did not fit the lighting slice; fall back to the
    /// single-block read (or fullbright) without panicking.
    TooShort,
}

/// Tiny FMA helper so the combine reads clearly; no `unsafe`, no intrinsics.
trait AddAssignScaled {
    fn add_assign_scaled(&mut self, value: f32, scale: f32);
}
impl AddAssignScaled for f32 {
    #[inline]
    fn add_assign_scaled(&mut self, value: f32, scale: f32) {
        *self += value * scale;
    }
}

/// Compute a face's static lightmap, or `None` if the face is fullbright. A thin
/// wrapper over [`face_lightmap_dyn`] with no dynamic lights and neutral light
/// styles, so its output is the borrowed static byte slice (byte-identical to the
/// pre-dlight behaviour).
///
/// The renderer always calls [`face_lightmap_dyn`] directly (threading its live
/// `dlights` and style scales); this wrapper is retained for the lightmap unit
/// tests, which assert the static-borrow path is unchanged.
#[cfg(test)]
fn face_lightmap<'a>(
    bsp: &'a Bsp,
    face: &crate::bsp::DFace,
    world_poly: &[Vec3],
) -> Option<LightMap<'a>> {
    face_lightmap_dyn(bsp, face, world_poly, &NEUTRAL_LIGHTSTYLE_SCALES, &[])
}

/// Compute a face's lightmap: the multi-style combine (`R_BuildLightMap`) scaled
/// by `light_styles`, plus any dynamic lights in `dlights` that reach the face
/// (`R_AddDynamicLights`). Returns `None` if the face is fullbright.
///
/// A face is fullbright when there is no `lighting` lump, the face has no
/// lightmap (`lightofs < 0`), the surface is special (sky/liquid — `TEX_SPECIAL`),
/// or the computed luxel grid would not fit in the remaining `lighting` slice.
///
/// `light_styles` is the per-style brightness scale (`1.0` == normal,
/// [`NEUTRAL_LIGHTSTYLE_SCALES`] disables animation). A face's `styles[0..3]`
/// (255 == unused) select which scales apply. For a single steady style-0 face at
/// the neutral scale (and no reaching dynamic light) the returned [`LightMap`]
/// *borrows* the static `Bsp::lighting` bytes, so the sampled factor — and the
/// rendered pixels — are byte-identical to the pre-style renderer. Otherwise the
/// [`LightMap`] owns an `f32` grid: `(sum of style blocks * style scale)` plus any
/// dynamic-light contributions, clamped by `factor_at`.
fn face_lightmap_dyn<'a>(
    bsp: &'a Bsp,
    face: &crate::bsp::DFace,
    world_poly: &[Vec3],
    light_styles: &[f32; LIGHTSTYLES],
    dlights: &[crate::dlight::DynamicLight],
) -> Option<LightMap<'a>> {
    use crate::bsp::TEX_SPECIAL;

    if bsp.lighting.is_empty() || face.lightofs < 0 {
        return None;
    }
    let ti = (face.texinfo as i64)
        .try_into()
        .ok()
        .and_then(|i: usize| bsp.texinfo.get(i))?;
    if ti.flags & TEX_SPECIAL != 0 {
        return None;
    }

    let (texmins, extent) = surface_extents(ti, world_poly)?;

    // Luxel grid dimensions: one luxel per 16-unit block, plus one.
    let lmw = (extent[0] / 16 + 1) as usize;
    let lmh = (extent[1] / 16 + 1) as usize;
    let count = lmw.checked_mul(lmh)?;

    let start: usize = face.lightofs.try_into().ok()?;
    // The style-0 (first) block must always fit; this is the static-borrow slice
    // and the fallback when the multi-style range does not fit.
    let samples = bsp.lighting.get(start..start.checked_add(count)?)?;

    let texmins_f = [texmins[0] as f32, texmins[1] as f32];

    // Combine the active style blocks (R_BuildLightMap). `StaticBlock` means the
    // common steady style-0-at-normal case: keep borrowing the static bytes.
    // `TooShort` means the multi-block range overran the lighting slice: fall back
    // to the single static block rather than going fullbright or panicking.
    let base: Option<Vec<f32>> =
        match build_styled_luxels(face, &bsp.lighting, start, count, light_styles) {
            StyleCombine::StaticBlock | StyleCombine::TooShort => None,
            StyleCombine::Combined(buf) => Some(buf),
        };

    // Add any reaching dynamic lights on top of the (possibly style-combined)
    // base. When `base` is None and no light reaches (or `dlights` is empty), the
    // result is None and we keep the byte-identical static borrow.
    let luxels = match add_dynamic_lights(bsp, face, ti, texmins_f, lmw, lmh, samples, base, dlights) {
        Some(owned) => Luxels::Owned(owned),
        None => Luxels::Static(samples),
    };

    Some(LightMap {
        luxels,
        lmw,
        lmh,
        texmins: texmins_f,
    })
}

/// Quake's `CalcSurfaceExtents`: returns `(texmins, extent)` in surface texels
/// for the given texinfo and world-space polygon.
///
/// For each axis `j` in 0..2 the surface coordinate of every vertex `p` is
/// `p·vecs[j].xyz + vecs[j][3]`; tracking its min/max gives
/// `texmins[j] = floor(min/16)*16` and `extent[j] = (ceil(max/16) - floor(min/16))*16`.
fn surface_extents(ti: &crate::bsp::TexInfo, world_poly: &[Vec3]) -> Option<([i32; 2], [i32; 2])> {
    if world_poly.len() < 3 {
        return None;
    }
    let mut mins = [f32::INFINITY; 2];
    let mut maxs = [f32::NEG_INFINITY; 2];
    for p in world_poly {
        for j in 0..2 {
            let val =
                p[0] * ti.vecs[j][0] + p[1] * ti.vecs[j][1] + p[2] * ti.vecs[j][2] + ti.vecs[j][3];
            if val < mins[j] {
                mins[j] = val;
            }
            if val > maxs[j] {
                maxs[j] = val;
            }
        }
    }
    let mut texmins = [0i32; 2];
    let mut extent = [0i32; 2];
    for j in 0..2 {
        if !mins[j].is_finite() || !maxs[j].is_finite() {
            return None;
        }
        let bmin = (mins[j] / 16.0).floor() as i32;
        let bmax = (maxs[j] / 16.0).ceil() as i32;
        texmins[j] = bmin.checked_mul(16)?;
        let ext = bmax.checked_sub(bmin)?.checked_mul(16)?;
        if ext < 0 {
            return None;
        }
        extent[j] = ext;
    }
    Some((texmins, extent))
}

// ---------------------------------------------------------------------------
// Animated special surfaces: liquid turbulent warp + scrolling sky
// ---------------------------------------------------------------------------
//
// Quake's `TEX_SPECIAL` faces (liquids and sky) are not lightmapped — they are
// drawn fullbright and *animated* every frame. This port reproduces two of
// those animations against the same perspective-correct `(s,t)` the textured
// rasteriser already interpolates:
//
//  * **Liquids** (miptex name begins with `*`: `*water1`, `*lava1`, `*slime`,
//    `*teleport`, …) get the SIN warp of `R_DrawTurbulent` / `EmitWaterPolys`
//    (`gl_warp.c`): each axis of the sample is displaced by a sine of the OTHER
//    axis plus time. See [`TurbTable`] / [`warp_st`].
//  * **Sky** (miptex name begins with `sky`: `sky1`, `sky4`, …) gets the
//    two-layer SCROLL of `EmitBothSkyLayers` (`gl_warp.c`) over the 256x128 sky
//    miptexture (two side-by-side 128x128 layers). See [`sky_texel`].

/// `gl_warp.c`'s `TURBSCALE = 256/(2*pi)`: scales a surface coordinate into the
/// 256-entry sine table's index space.
const TURBSCALE: f32 = 256.0 / (2.0 * std::f32::consts::PI);

/// The warp amplitude in texels (`AMP` in `gl_warp_sin.h` — the WinQuake float
/// `turbsin[]` table swings ±8 texels).
const TURB_AMP: f32 = 8.0;

/// The 256-entry `turbsin` table from `gl_warp.c`: `turbsin[i] = AMP*sin(i*2pi/256)`.
///
/// Built once per render (a plain `[f32; 256]`, no `lazy_static`) and borrowed
/// by the rasteriser. Indexing is masked to `& 255`, so any input is in range.
struct TurbTable {
    sin: [f32; 256],
}

impl TurbTable {
    /// Compute the table. `const`-friendly arithmetic, but `f32::sin` is not yet
    /// `const`, so this runs once at render start.
    fn new() -> TurbTable {
        let mut sin = [0.0f32; 256];
        let mut i = 0usize;
        while i < 256 {
            sin[i] = TURB_AMP * ((i as f32) * 2.0 * std::f32::consts::PI / 256.0).sin();
            i += 1;
        }
        TurbTable { sin }
    }

    /// `turbsin[(int)(coord * TURBSCALE) & 255]` — the displacement (in texels)
    /// applied to one axis as a function of the other axis + time.
    #[inline]
    fn at(&self, coord: f32) -> f32 {
        // `& 255` on the truncated index keeps it in `[0,255]` for any finite
        // input; non-finite inputs fall back to index 0.
        let raw = coord * TURBSCALE;
        let idx = if raw.is_finite() { (raw as i64) & 255 } else { 0 };
        self.sin[idx as usize]
    }
}

/// Apply the liquid SIN warp to a surface coordinate `(s,t)` at game `time`,
/// returning the displaced `(s2,t2)` to sample, exactly as `EmitWaterPolys`:
///
/// ```text
/// s2 = s + turbsin[(int)((t*0.125 + time) * TURBSCALE) & 255]
/// t2 = t + turbsin[(int)((s*0.125 + time) * TURBSCALE) & 255]
/// ```
///
/// Each axis is offset by a sine of the *other* axis plus time, so the surface
/// appears to ripple. The caller still wraps `(s2,t2)` into the (tiling) texture
/// via `rem_euclid`.
#[inline]
fn warp_st(turb: &TurbTable, s: f32, t: f32, time: f32) -> (f32, f32) {
    let s2 = s + turb.at(t * 0.125 + time);
    let t2 = t + turb.at(s * 0.125 + time);
    (s2, t2)
}

/// Sample one texel of the two-layer scrolling sky from a 256x128 sky
/// miptexture, porting `EmitBothSkyLayers` (`gl_warp.c`) / `R_InitSky`.
///
/// The sky miptexture is `tw=256` wide, `th=128` tall: two side-by-side
/// 128x128 layers. Per `R_InitSky`, the **right** half (`[128,256)`) is the
/// solid background layer, and the **left** half (`[0,128)`) is the alpha
/// overlay whose palette index `0` is transparent (showing the background
/// through it). `EmitBothSkyLayers` scrolls the background at `time*8` and the
/// overlay at `time*16` (twice as fast). Here the perspective-correct surface
/// `(s,t)` plays the role of the GL sky direction: it is scaled down and the
/// per-layer scroll offset added, then wrapped into each 128x128 layer.
///
/// Returns a palette index. Every lookup is `.get()`-guarded and wrapped with
/// `rem_euclid`, so a malformed (non-256x128) sky texture never panics: it
/// simply samples whatever is in range, and a too-small texture yields index 0.
#[inline]
fn sky_texel(pixels: &[u8], tw: usize, th: usize, s: f32, t: f32, time: f32) -> u8 {
    // Layer dimension: the texture is conceptually two `lh`-wide square layers.
    // Use half the width (clamped to the height) so a real 256x128 sky gives
    // 128x128 layers; degenerate sizes still stay in range via the wraps below.
    let lw = (tw / 2).max(1);
    let lh = th.max(1);

    // The surface (s,t) stand in for the GL sky direction; scale them down so a
    // wall's worth of texels maps across the layer rather than tiling violently.
    // (1/8 keeps the cloud features a sensible on-screen size.)
    let bs = s * 0.125;
    let bt = t * 0.125;

    // Background (solid) layer: right half, scroll = time*8.
    let back = {
        let sx = ((bs + time * 8.0) as i64).rem_euclid(lw as i64) as usize;
        let sy = (bt as i64).rem_euclid(lh as i64) as usize;
        // Right half starts at column `lw` (= 128 for a real sky).
        pixels.get(sy * tw + (lw + sx)).copied().unwrap_or(0)
    };

    // Overlay (alpha) layer: left half, scroll = time*16. Palette index 0 is
    // transparent — where transparent, the background shows through.
    let front = {
        let sx = ((bs + time * 16.0) as i64).rem_euclid(lw as i64) as usize;
        let sy = (bt as i64).rem_euclid(lh as i64) as usize;
        pixels.get(sy * tw + sx).copied().unwrap_or(0)
    };

    if front != 0 {
        front
    } else {
        back
    }
}

/// How the per-pixel `(s,t)` -> texel step of [`raster_triangle_tex`] behaves.
///
/// `Normal` is the existing wall path (optional lightmap). `Turb` and `Sky`
/// drive the animated special-surface sampling above; both are drawn fullbright
/// (Quake never lightmaps liquids or sky), so they ignore the `lightmap`/`shade`
/// brightness inputs and the rasteriser applies a fixed unit brightness.
#[derive(Clone, Copy)]
enum SurfaceMode<'a> {
    /// Ordinary wall: sample `pixels` at the interpolated `(s,t)`.
    Normal,
    /// Liquid: SIN-warp `(s,t)` by `time` before sampling (fullbright).
    Turb { turb: &'a TurbTable, time: f32 },
    /// Sky: two-layer scroll over the 256x128 sky texture by `time` (fullbright).
    Sky { time: f32 },
}

/// Which animated kind a miptexture name selects: liquids begin with `*`
/// (`*water1`, `*lava1`, `*slime`, `*teleport`), sky begins with `sky`
/// (`sky1`, `sky4`); anything else is an ordinary lightmapped wall.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SurfKind {
    Normal,
    Turb,
    Sky,
}

/// Classify a miptexture by its name, exactly as Quake's `Mod_LoadFaces`
/// flags surfaces (`*`-prefixed -> `SURF_DRAWTURB`, `sky`-prefixed ->
/// `SURF_DRAWSKY`). The match is ASCII case-insensitive on the `sky` prefix to
/// tolerate `SKY1`-style names; the `*` check is exact.
fn classify_surface(name: &str) -> SurfKind {
    if name.starts_with('*') {
        SurfKind::Turb
    } else if name.len() >= 3 && name.as_bytes()[..3].eq_ignore_ascii_case(b"sky") {
        SurfKind::Sky
    } else {
        SurfKind::Normal
    }
}

/// A projected vertex carrying texture coordinates for perspective-correct
/// sampling. `vz` is forward depth (used linearly for the z-buffer, to stay
/// consistent with the flat path); `s`/`t` are Quake surface texel coordinates
/// (they also index the face lightmap, which shares the texinfo axes).
#[derive(Clone, Copy)]
struct ProjT {
    x: f32,
    y: f32,
    vz: f32,
    s: f32,
    t: f32,
}

/// Perspective-correct textured triangle. `pixels` is `tw * th` palette indices.
/// The z-buffer uses linearly-interpolated `vz` (matching [`raster_triangle`]),
/// while `s`/`t` are interpolated with perspective correction (`s/z`, `1/z`).
#[allow(clippy::too_many_arguments)]
fn raster_triangle_tex(
    image: &mut Image,
    zbuf: &mut [f32],
    v0: ProjT,
    v1: ProjT,
    v2: ProjT,
    pixels: &[u8],
    tw: usize,
    th: usize,
    palette: &[[u8; 3]; 256],
    shade: f32,
    lightmap: Option<&LightMap>,
    mode: SurfaceMode,
) {
    let w = image.w;
    let h = image.h;
    if w == 0 || h == 0 || tw == 0 || th == 0 || pixels.len() < tw * th {
        return;
    }
    let min_xf = v0.x.min(v1.x).min(v2.x);
    let max_xf = v0.x.max(v1.x).max(v2.x);
    let min_yf = v0.y.min(v1.y).min(v2.y);
    let max_yf = v0.y.max(v1.y).max(v2.y);
    if !(min_xf.is_finite() && max_xf.is_finite() && min_yf.is_finite() && max_yf.is_finite()) {
        return;
    }
    let min_x = min_xf.floor().max(0.0) as i64;
    let max_x = max_xf.ceil().min((w as i64 - 1) as f32) as i64;
    let min_y = min_yf.floor().max(0.0) as i64;
    let max_y = max_yf.ceil().min((h as i64 - 1) as f32) as i64;
    if min_x > max_x || min_y > max_y {
        return;
    }
    let area = edge(v0.x, v0.y, v1.x, v1.y, v2.x, v2.y);
    if area.abs() < 1e-6 {
        return;
    }
    let inv_area = 1.0 / area;
    let (iz0, iz1, iz2) = (1.0 / v0.vz, 1.0 / v1.vz, 1.0 / v2.vz);
    let (soz0, soz1, soz2) = (v0.s * iz0, v1.s * iz1, v2.s * iz2);
    let (toz0, toz1, toz2) = (v0.t * iz0, v1.t * iz1, v2.t * iz2);

    for py in min_y..=max_y {
        for px in min_x..=max_x {
            let sx = px as f32 + 0.5;
            let sy = py as f32 + 0.5;
            let w0 = edge(v1.x, v1.y, v2.x, v2.y, sx, sy) * inv_area;
            let w1 = edge(v2.x, v2.y, v0.x, v0.y, sx, sy) * inv_area;
            let w2 = edge(v0.x, v0.y, v1.x, v1.y, sx, sy) * inv_area;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let depth = w0 * v0.vz + w1 * v1.vz + w2 * v2.vz; // linear, like the flat path
            let idx = (py as usize) * w + (px as usize);
            let zc = match zbuf.get_mut(idx) {
                Some(z) => z,
                None => continue,
            };
            if depth >= *zc {
                continue;
            }
            let inv_z = w0 * iz0 + w1 * iz1 + w2 * iz2;
            if inv_z <= 0.0 {
                continue;
            }
            let s = (w0 * soz0 + w1 * soz1 + w2 * soz2) / inv_z;
            let t = (w0 * toz0 + w1 * toz1 + w2 * toz2) / inv_z;

            // Resolve the palette index and per-pixel brightness per surface
            // mode. Liquids/sky are fullbright (brightness 1.0, no lightmap);
            // walls keep the lightmap-or-`shade` brightness.
            let (texel, brightness) = match mode {
                SurfaceMode::Normal => {
                    let tx = (s as i64).rem_euclid(tw as i64) as usize;
                    let ty = (t as i64).rem_euclid(th as i64) as usize;
                    let p = match pixels.get(ty * tw + tx) {
                        Some(&p) => p as usize,
                        None => continue,
                    };
                    // A baked lightmap (indexed by the same surface (s,t), which
                    // shares the texinfo axes) replaces the flat Lambert `shade`.
                    let b = match lightmap {
                        Some(lm) => lm.factor_at(s, t),
                        None => shade,
                    };
                    (p, b)
                }
                SurfaceMode::Turb { turb, time } => {
                    // SIN-warp the (s,t) before the (tiling) wrap; fullbright.
                    let (s2, t2) = warp_st(turb, s, t, time);
                    let tx = (s2 as i64).rem_euclid(tw as i64) as usize;
                    let ty = (t2 as i64).rem_euclid(th as i64) as usize;
                    let p = match pixels.get(ty * tw + tx) {
                        Some(&p) => p as usize,
                        None => continue,
                    };
                    (p, 1.0)
                }
                SurfaceMode::Sky { time } => {
                    // Two-layer scrolling sky; fullbright. `sky_texel` does its
                    // own bounds-checked wrapping over the 256x128 layout.
                    (sky_texel(pixels, tw, th, s, t, time) as usize, 1.0)
                }
            };
            let rgb = palette[texel];
            *zc = depth;
            if let Some(p) = image.rgb.get_mut(idx) {
                *p = [
                    (rgb[0] as f32 * brightness).clamp(0.0, 255.0) as u8,
                    (rgb[1] as f32 * brightness).clamp(0.0, 255.0) as u8,
                    (rgb[2] as f32 * brightness).clamp(0.0, 255.0) as u8,
                ];
            }
        }
    }
}

/// Render `bsp` with its real miptextures sampled through `palette` (Quake's
/// `gfx/palette.lmp`). Texture coordinates come from each face's `texinfo` axes;
/// sampling is perspective-correct. Faces whose texture has no inline pixels
/// fall back to the flat hashed colour of [`render_bsp`]. Same view transform,
/// projection, near clip, and backface cull as [`render_bsp`].
pub fn render_bsp_textured(
    bsp: &Bsp,
    cam: &Camera,
    w: usize,
    h: usize,
    palette: &[[u8; 3]; 256],
) -> Image {
    let mut image = Image::new(w, h, [10, 10, 14]);
    if w == 0 || h == 0 {
        return image;
    }
    let mut zbuf = vec![f32::INFINITY; w.saturating_mul(h)];
    // Static (time 0) world: liquids/sky show their texture but do not advance.
    let turb = TurbTable::new();
    draw_world_textured(&mut image, &mut zbuf, bsp, cam, palette, &turb, 0.0, &NEUTRAL_LIGHTSTYLE_SCALES, &[]);
    image
}

// ---------------------------------------------------------------------------
// PVS culling (potentially-visible set)
// ---------------------------------------------------------------------------
//
// Ports three pieces of Quake's visibility pipeline:
//   * `Mod_DecompressVis` (model.c): run-length-decode the per-leaf PVS bitset.
//   * `Mod_PointInLeaf` (model.c): walk the BSP node tree to the leaf a point
//     falls in.
//   * the leaf-marking core of `R_MarkLeaves` (r_main.c): expand the PVS into a
//     per-face "visible" set via each visible leaf's marksurfaces.
//
// As everywhere in this module, every index into BSP-derived data is checked;
// malformed data degrades to "draw everything" (the safe, non-culling default)
// rather than panicking.

/// Run-length-decode a leaf's compressed PVS, starting at byte `visofs` in
/// `model_vis` (the raw `LUMP_VISIBILITY` bytes).
///
/// Quake's RLE: a non-zero byte carries eight leaf-visibility bits directly
/// (LSB first); a `0` byte is followed by a second byte giving a run length of
/// *zero* bytes to emit (i.e. that many leaves not visible). Decoding stops once
/// `numleafs` leaves have been produced. Mirrors `Mod_DecompressVis`.
///
/// Returns a `Vec<bool>` of length `numleafs + 1` indexed by leaf number; leaf 0
/// (the shared solid/outside leaf) has no meaningful bit and is left `false`.
/// When `visofs < 0` (no vis info for this leaf) every leaf is reported visible,
/// matching the C `decompressed = mod_novis` all-ones fallback.
fn decompress_vis(model_vis: &[u8], visofs: i32, numleafs: usize) -> Vec<bool> {
    // The PVS describes leaves 1..=numleafs; index 0 is the solid leaf. Size the
    // bitset to numleafs+1 so callers can index by leaf number directly.
    let out_len = numleafs.saturating_add(1);

    // No vis info -> everything visible (Quake's `mod_novis`).
    let start: usize = match usize::try_from(visofs) {
        Ok(s) => s,
        Err(_) => return vec![true; out_len],
    };

    let mut out = vec![false; out_len];
    let mut pos = start;
    // `row` counts how many leaf bits we have produced so far. The C writes the
    // decompressed bits starting at out[0]; we offset by 1 so out[L] is leaf L
    // (leaf 0 stays false). Quake decompresses `(numleafs+7)>>3` bytes worth.
    let mut leaf: usize = 1;

    while leaf <= numleafs {
        let byte = match model_vis.get(pos) {
            Some(&b) => b,
            // Ran off the end of the vis lump: stop (remaining leaves stay
            // not-visible). Never indexes out of range.
            None => break,
        };
        pos += 1;

        if byte != 0 {
            // Eight visibility bits, LSB = lowest leaf number.
            let mut bit = 1u8;
            for _ in 0..8 {
                if leaf > numleafs {
                    break;
                }
                if byte & bit != 0 {
                    if let Some(slot) = out.get_mut(leaf) {
                        *slot = true;
                    }
                }
                leaf += 1;
                bit <<= 1;
            }
        } else {
            // A zero byte: the next byte is a count of zero-bytes (8 leaves each)
            // to skip. A truncated run (no count byte) simply stops decoding.
            let count = match model_vis.get(pos) {
                Some(&c) => c as usize,
                None => break,
            };
            pos += 1;
            // Advance over `count` zero bytes = 8*count not-visible leaves.
            leaf = leaf.saturating_add(count.saturating_mul(8));
        }
    }

    out
}

/// Walk the worldmodel's BSP node tree to find which leaf the world-space point
/// `p` falls in, porting `Mod_PointInLeaf`.
///
/// Starts at `models[0].headnode[0]` (a node index). At each node the point is
/// classified against the node's plane: `dot(normal, p) - dist >= 0` takes
/// `children[0]` (front), otherwise `children[1]` (back). A *negative* child
/// encodes a leaf as `-(child) - 1`; a non-negative child is the next node.
///
/// Returns the leaf index, or `None` if the model/headnode/plane/child indices
/// are malformed or out of range (every access is bounds-checked, so this never
/// panics on corrupt data). A bounded iteration guard prevents a cyclic/corrupt
/// node graph from looping forever.
fn point_in_leaf(bsp: &Bsp, p: Vec3) -> Option<usize> {
    let model = bsp.models.first()?;
    // headnode[0] is the rendering hull's root node index.
    let mut node_index: i32 = *model.headnode.first()?;

    // A valid descent visits at most `nodes.len()` nodes; cap iterations a bit
    // above that to defend against a malformed (cyclic) node graph.
    let max_steps = bsp.nodes.len().saturating_add(1);
    for _ in 0..=max_steps {
        if node_index < 0 {
            // Leaf: leaf index = -(node_index) - 1.
            let leaf = (-1 - node_index) as i64; // node_index < 0 => non-negative
            let leaf_index: usize = leaf.try_into().ok()?;
            // Confirm it is a real leaf so callers can index `bsp.leafs` safely.
            if leaf_index < bsp.leafs.len() {
                return Some(leaf_index);
            }
            return None;
        }

        let ni: usize = node_index.try_into().ok()?;
        let node = bsp.nodes.get(ni)?;
        let pi: usize = (node.planenum as i64).try_into().ok()?;
        let plane = bsp.planes.get(pi)?;

        let d = dot(plane.normal, p) - plane.dist;
        // front (child[0]) when on/in front of the plane, else back (child[1]).
        let child = if d >= 0.0 {
            *node.children.first()?
        } else {
            *node.children.get(1)?
        };
        node_index = child as i32;
    }

    // Exceeded the step guard: treat as malformed.
    None
}

/// Build a per-face visibility mask for the camera at `cam_pos`, porting the
/// leaf-marking core of `R_MarkLeaves`.
///
/// Returns `None` (meaning "draw everything, no culling") when there is no
/// usable PVS for the camera: an empty visibility lump, no leafs, the camera
/// resolving to leaf 0 (the solid/outside leaf), or a malformed BSP. Otherwise
/// returns a `Vec<bool>` of length `faces.len()` where `true` marks a face that
/// must be drawn.
///
/// Faces reached through visible leaves' `marksurfaces` are marked visible. Any
/// face *not* referenced by some leaf's marksurfaces (e.g. submodel faces, which
/// belong to brush entities rather than the worldmodel's leaves) is left visible
/// too, so submodels always draw. Out-of-range marksurface/leaf indices are
/// skipped harmlessly (they simply fail to mark, never panic).
fn compute_visible_faces(bsp: &Bsp, cam_pos: Vec3) -> Option<Vec<bool>> {
    if bsp.visibility.is_empty() || bsp.leafs.is_empty() || bsp.faces.is_empty() {
        return None;
    }

    let view_leaf = point_in_leaf(bsp, cam_pos)?;
    // Leaf 0 is the solid/outside leaf (no PVS) — draw everything.
    if view_leaf == 0 {
        return None;
    }
    let leaf = bsp.leafs.get(view_leaf)?;
    if leaf.visofs < 0 {
        // This leaf carries no vis info — draw everything.
        return None;
    }

    // numleafs for the PVS is the visible-leaf count (leaves 1..=numleafs).
    let numleafs = bsp.leafs.len().saturating_sub(1);
    let vis = decompress_vis(&bsp.visibility, leaf.visofs, numleafs);

    // Start by marking every face that no leaf claims (submodels etc.) visible,
    // and every leaf-owned face not-visible; then re-mark the PVS-visible ones.
    // We discover "leaf-owned" faces in the same pass: a face becomes leaf-owned
    // the first time any leaf's marksurfaces references it.
    let nfaces = bsp.faces.len();
    let mut leaf_owned = vec![false; nfaces];
    let mut visible = vec![false; nfaces];

    for (li, lf) in bsp.leafs.iter().enumerate() {
        let first = lf.firstmarksurface as usize;
        let count = lf.nummarksurfaces as usize;
        let end = match first.checked_add(count) {
            Some(e) => e,
            None => continue,
        };
        // Slice the marksurfaces span for this leaf; out-of-range spans are
        // skipped (the leaf simply contributes no marks).
        let marks = match bsp.marksurfaces.get(first..end) {
            Some(m) => m,
            None => continue,
        };
        // Is this leaf in the PVS of the view leaf? (Leaf 0 / out-of-range -> no.)
        let leaf_visible = vis.get(li).copied().unwrap_or(false);
        for &ms in marks {
            let fi = ms as usize;
            if let Some(owned) = leaf_owned.get_mut(fi) {
                *owned = true;
            }
            if leaf_visible {
                if let Some(v) = visible.get_mut(fi) {
                    *v = true;
                }
            }
        }
    }

    // Any face never owned by a leaf (submodel faces) draws unconditionally.
    for fi in 0..nfaces {
        if !leaf_owned.get(fi).copied().unwrap_or(true) {
            if let Some(v) = visible.get_mut(fi) {
                *v = true;
            }
        }
    }

    Some(visible)
}

/// The textured world pass, factored out of [`render_bsp_textured`] so it can
/// share an image + z-buffer with the alias-model pass (see [`render_scene`]).
///
/// Rasterises every visible BSP face into `image`/`zbuf` exactly as the original
/// `render_bsp_textured` body did: same camera basis, focal length, projection,
/// near clip, backface cull, perspective-correct texturing, and flat fallback.
/// The caller owns the framebuffers, so models drawn afterward occlude (and are
/// occluded by) the world through the shared depth buffer.
///
/// Before the per-face loop it computes the camera's PVS via
/// [`compute_visible_faces`]: when the map has visibility data and the camera is
/// in a real (non-solid) leaf, faces outside the potentially-visible set are
/// skipped. Maps with no visibility lump (e.g. [`demo_room`]) get the full draw,
/// so existing behaviour is unchanged there.
#[allow(clippy::too_many_arguments)]
fn draw_world_textured(
    image: &mut Image,
    zbuf: &mut [f32],
    bsp: &Bsp,
    cam: &Camera,
    palette: &[[u8; 3]; 256],
    turb: &TurbTable,
    time: f32,
    light_styles: &[f32; LIGHTSTYLES],
    dlights: &[crate::dlight::DynamicLight],
) {
    const NEAR: f32 = 1.0;
    let (w, h) = (image.w, image.h);
    if w == 0 || h == 0 {
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
    let (light_dir, _l) = normalize([0.3, 0.5, 1.0]);

    // PVS culling: a per-face visibility mask for the camera's leaf, or `None`
    // when there is no usable PVS (no vis lump, solid/outside leaf, malformed) —
    // in which case every face is drawn (the pre-PVS behaviour).
    let visible_face = compute_visible_faces(bsp, cam.pos);

    // The world pass draws ONLY model 0's faces. Brush submodels (doors, plats,
    // buttons) own the remaining faces and are drawn by `draw_submodel` at their
    // entity origin — otherwise they'd render here at their local (closed)
    // position AND again, doubled, at the entity origin. Fall back to "all faces"
    // when models[0] is absent (the pre-submodel behaviour / demo maps).
    let (world_first, world_end) = match bsp.models.first() {
        Some(m) => {
            let f0 = m.firstface.max(0) as usize;
            let n = m.numfaces.max(0) as usize;
            (f0, f0.saturating_add(n).min(bsp.faces.len()))
        }
        None => (0, bsp.faces.len()),
    };

    let mut world_poly: Vec<Vec3> = Vec::new();
    let mut proj: Vec<ProjT> = Vec::new();

    for face_index in world_first..world_end {
        let face = match bsp.faces.get(face_index) {
            Some(f) => f,
            None => continue,
        };
        // Skip faces outside the potentially-visible set. A missing mask entry
        // (or no mask at all) means "draw" — culling never removes a face it is
        // unsure about.
        if let Some(mask) = &visible_face {
            if !mask.get(face_index).copied().unwrap_or(true) {
                continue;
            }
        }

        if !face_world_poly(bsp, face, &mut world_poly) {
            continue;
        }
        let normal = match face_normal(bsp, face) {
            Some(n) => n,
            None => continue,
        };

        // Face center for the cull test.
        let mut center = [0.0f32; 3];
        for v in &world_poly {
            for k in 0..3 {
                center[k] += v[k];
            }
        }
        let inv_n = 1.0 / world_poly.len() as f32;
        for k in 0..3 {
            center[k] *= inv_n;
        }
        if dot(normal, sub(center, cam.pos)) >= 0.0 {
            continue;
        }

        // texinfo (s/t axes) and its miptexture.
        let ti = (face.texinfo as i64)
            .try_into()
            .ok()
            .and_then(|i: usize| bsp.texinfo.get(i));
        let tex = ti.and_then(|t| {
            let mi: usize = t.miptex.try_into().ok()?;
            bsp.textures.get(mi).and_then(|o| o.as_ref())
        });

        // Classify the surface (liquid / sky / wall) by its miptex name so the
        // animated special surfaces route to the warp/scroll sampler. Liquids
        // and sky are fullbright and NOT lightmapped, so only walls compute a
        // baked static lightmap.
        let kind = tex.map(|mt| classify_surface(&mt.name)).unwrap_or(SurfKind::Normal);
        let lightmap = if kind == SurfKind::Normal {
            face_lightmap_dyn(bsp, face, &world_poly, light_styles, dlights)
        } else {
            None
        };
        let mode = match kind {
            SurfKind::Normal => SurfaceMode::Normal,
            SurfKind::Turb => SurfaceMode::Turb { turb, time },
            SurfKind::Sky => SurfaceMode::Sky { time },
        };

        // Project, computing texel coords from the texinfo axes.
        proj.clear();
        let mut clipped = false;
        for v in &world_poly {
            let rel = sub(*v, cam.pos);
            let vz = dot(rel, forward);
            if vz <= NEAR {
                clipped = true;
                break;
            }
            let vx = dot(rel, right);
            let vy = dot(rel, up);
            let (s, t) = match ti {
                Some(ti) => (
                    v[0] * ti.vecs[0][0] + v[1] * ti.vecs[0][1] + v[2] * ti.vecs[0][2] + ti.vecs[0][3],
                    v[0] * ti.vecs[1][0] + v[1] * ti.vecs[1][1] + v[2] * ti.vecs[1][2] + ti.vecs[1][3],
                ),
                None => (0.0, 0.0),
            };
            proj.push(ProjT {
                x: cx + focal * vx / vz,
                y: cy - focal * vy / vz,
                vz,
                s,
                t,
            });
        }
        if clipped || proj.len() < 3 {
            continue;
        }

        let lambert = dot(normal, light_dir).max(0.0);
        let shade = (0.5 + 0.5 * lambert).min(1.0);

        match tex {
            Some(mt) if !mt.pixels.is_empty() && mt.width > 0 && mt.height > 0 => {
                let (tw, th) = (mt.width as usize, mt.height as usize);
                let v0 = proj[0];
                for i in 1..proj.len() - 1 {
                    raster_triangle_tex(
                        image, zbuf, v0, proj[i], proj[i + 1],
                        &mt.pixels, tw, th, palette, shade, lightmap.as_ref(), mode,
                    );
                }
            }
            _ => {
                // Flat hashed colour for textureless faces (e.g. sky/animated).
                // When the face has a lightmap, route through the textured path
                // with a 1x1 single-colour texture so the per-pixel lightmap
                // factor is applied; otherwise keep the original flat shading.
                let key = ti.map(|t| t.miptex as i64).unwrap_or(face.texinfo as i64);
                let base = hash_color(key);
                match lightmap {
                    Some(lm) => {
                        // A 1x1 texture whose single index maps to the hashed
                        // base colour (before any shade); brightness comes from
                        // the lightmap inside the rasteriser.
                        let mut pal1 = [[0u8; 3]; 256];
                        pal1[0] = [
                            (base[0] * 255.0).clamp(0.0, 255.0) as u8,
                            (base[1] * 255.0).clamp(0.0, 255.0) as u8,
                            (base[2] * 255.0).clamp(0.0, 255.0) as u8,
                        ];
                        let one = [0u8];
                        let v0 = proj[0];
                        for i in 1..proj.len() - 1 {
                            raster_triangle_tex(
                                image, zbuf, v0, proj[i], proj[i + 1],
                                &one, 1, 1, &pal1, shade, Some(&lm), SurfaceMode::Normal,
                            );
                        }
                    }
                    None => {
                        let color = [
                            (base[0] * shade * 255.0).clamp(0.0, 255.0) as u8,
                            (base[1] * shade * 255.0).clamp(0.0, 255.0) as u8,
                            (base[2] * shade * 255.0).clamp(0.0, 255.0) as u8,
                        ];
                        let p0 = Projected { x: proj[0].x, y: proj[0].y, depth: proj[0].vz };
                        for i in 1..proj.len() - 1 {
                            let p1 = Projected { x: proj[i].x, y: proj[i].y, depth: proj[i].vz };
                            let p2 =
                                Projected { x: proj[i + 1].x, y: proj[i + 1].y, depth: proj[i + 1].vz };
                            raster_triangle(image, zbuf, p0, p1, p2, color);
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Brush submodels (doors, platforms, buttons — Quake's inline `*N` bmodels)
// ---------------------------------------------------------------------------

/// Draw one brush submodel (`bsp.models[model_index]`) at world `origin`, sharing
/// `image`/`zbuf` so it occludes — and is occluded by — the world and other
/// geometry through the one depth buffer.
///
/// Submodels (doors/plats/buttons referenced by brush entities whose `model`
/// field is `"*N"`) live in the same vertex/edge/surfedge arrays as the
/// worldmodel; [`face_world_poly`] therefore reconstructs each submodel face in
/// the *same* model-local coordinate space as the world. To place the submodel
/// we add `origin` to every reconstructed vertex.
///
/// This is a faithful variant of [`draw_world_textured`]'s per-face loop with
/// three deliberate differences:
///  * **No PVS cull.** Submodel faces are not referenced by any worldmodel
///    leaf's marksurfaces, so they have no PVS bit; they always draw (matching
///    `compute_visible_faces`, which leaves unowned faces visible).
///  * **Origin shift.** Each vertex is projected from its *origin-shifted* world
///    position, and the backface cull uses the *shifted* face center against
///    `cam.pos`. The plane normal is a direction (origin-independent), so
///    [`face_normal`] is used directly. Quake also backface-culls bmodel polys,
///    so a door not visible from a side is faithful.
///  * **Local ST / lightmap.** Quake computes a bmodel's surface `(s,t)` from the
///    model-space vertex (i.e. *before* the origin shift), and lightmap extents
///    come from the origin-independent texinfo `(s,t)` of the *local* polygon.
///    So `(s,t)` and the lightmap are derived from the LOCAL vertices while only
///    projection uses the shifted ones.
///
/// Every index into BSP-derived data is bounds-checked; a malformed face (or an
/// out-of-range `model_index`) is skipped, never panicked on.
#[allow(clippy::too_many_arguments)]
fn draw_submodel(
    image: &mut Image,
    zbuf: &mut [f32],
    bsp: &Bsp,
    cam: &Camera,
    palette: &[[u8; 3]; 256],
    model_index: usize,
    origin: Vec3,
    turb: &TurbTable,
    time: f32,
    light_styles: &[f32; LIGHTSTYLES],
    dlights: &[crate::dlight::DynamicLight],
) {
    const NEAR: f32 = 1.0;
    let (w, h) = (image.w, image.h);
    if w == 0 || h == 0 {
        return;
    }
    // Out-of-range submodel index is a silent no-op.
    let m = match bsp.models.get(model_index) {
        Some(m) => m,
        None => return,
    };

    // The submodel's lightmap is built from the LOCAL (pre-shift) polygon, so the
    // dynamic lights — which live in world space — are translated into the
    // submodel's local frame by subtracting `origin` (Quake's `R_DrawBrushModel`
    // does the same `VectorSubtract(dlight.origin, ent->origin, lightorigin)`).
    // An empty input stays empty, so a submodel with no dlights is unchanged.
    let local_dlights: Vec<crate::dlight::DynamicLight> = dlights
        .iter()
        .map(|dl| {
            let mut d = *dl;
            d.origin = [
                dl.origin[0] - origin[0],
                dl.origin[1] - origin[1],
                dl.origin[2] - origin[2],
            ];
            d
        })
        .collect();

    // Same camera basis / focal length / projection as the world pass.
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
    let (light_dir, _l) = normalize([0.3, 0.5, 1.0]);

    // Submodel face range: [firstface, firstface + numfaces). Negative counts
    // clamp to 0 so the range is empty rather than wrapping.
    let f0 = m.firstface.max(0) as usize;
    let count = m.numfaces.max(0) as usize;
    let end = match f0.checked_add(count) {
        Some(e) => e,
        None => return,
    };

    // `world_poly` holds origin-SHIFTED vertices (for projection); we keep the
    // LOCAL vertices separately for (s,t) and the lightmap.
    let mut local_poly: Vec<Vec3> = Vec::new();
    let mut world_poly: Vec<Vec3> = Vec::new();
    let mut proj: Vec<ProjT> = Vec::new();

    for face_index in f0..end {
        let face = match bsp.faces.get(face_index) {
            Some(f) => f,
            None => continue,
        };

        // Reconstruct the LOCAL polygon (same space as the worldmodel).
        if !face_world_poly(bsp, face, &mut local_poly) {
            continue;
        }
        // Origin-shifted copy used for the cull center and projection.
        world_poly.clear();
        for v in &local_poly {
            world_poly.push([v[0] + origin[0], v[1] + origin[1], v[2] + origin[2]]);
        }

        let normal = match face_normal(bsp, face) {
            Some(n) => n,
            None => continue,
        };

        // Backface cull against the SHIFTED face center.
        let mut center = [0.0f32; 3];
        for v in &world_poly {
            for k in 0..3 {
                center[k] += v[k];
            }
        }
        let inv_n = 1.0 / world_poly.len() as f32;
        for k in 0..3 {
            center[k] *= inv_n;
        }
        if dot(normal, sub(center, cam.pos)) >= 0.0 {
            continue;
        }

        // texinfo (s/t axes) and its miptexture.
        let ti = (face.texinfo as i64)
            .try_into()
            .ok()
            .and_then(|i: usize| bsp.texinfo.get(i));
        let tex = ti.and_then(|t| {
            let mi: usize = t.miptex.try_into().ok()?;
            bsp.textures.get(mi).and_then(|o| o.as_ref())
        });

        // Classify the surface (liquid / sky / wall) by its miptex name. Liquids
        // and sky are fullbright and NOT lightmapped; only walls compute a
        // lightmap (from the LOCAL polygon — texinfo extents are origin-independent).
        let kind = tex.map(|mt| classify_surface(&mt.name)).unwrap_or(SurfKind::Normal);
        let lightmap = if kind == SurfKind::Normal {
            face_lightmap_dyn(bsp, face, &local_poly, light_styles, &local_dlights)
        } else {
            None
        };
        let mode = match kind {
            SurfKind::Normal => SurfaceMode::Normal,
            SurfKind::Turb => SurfaceMode::Turb { turb, time },
            SurfKind::Sky => SurfaceMode::Sky { time },
        };

        // Project the SHIFTED vertices, but compute (s,t) from the LOCAL vertices.
        proj.clear();
        let mut clipped = false;
        for (vi, vw) in world_poly.iter().enumerate() {
            let rel = sub(*vw, cam.pos);
            let vz = dot(rel, forward);
            if vz <= NEAR {
                clipped = true;
                break;
            }
            let vx = dot(rel, right);
            let vy = dot(rel, up);
            // (s,t) from the local (pre-shift) vertex coordinate.
            let vl = match local_poly.get(vi) {
                Some(v) => *v,
                None => {
                    clipped = true;
                    break;
                }
            };
            let (s, t) = match ti {
                Some(ti) => (
                    vl[0] * ti.vecs[0][0] + vl[1] * ti.vecs[0][1] + vl[2] * ti.vecs[0][2] + ti.vecs[0][3],
                    vl[0] * ti.vecs[1][0] + vl[1] * ti.vecs[1][1] + vl[2] * ti.vecs[1][2] + ti.vecs[1][3],
                ),
                None => (0.0, 0.0),
            };
            proj.push(ProjT {
                x: cx + focal * vx / vz,
                y: cy - focal * vy / vz,
                vz,
                s,
                t,
            });
        }
        if clipped || proj.len() < 3 {
            continue;
        }

        let lambert = dot(normal, light_dir).max(0.0);
        let shade = (0.5 + 0.5 * lambert).min(1.0);

        match tex {
            Some(mt) if !mt.pixels.is_empty() && mt.width > 0 && mt.height > 0 => {
                let (tw, th) = (mt.width as usize, mt.height as usize);
                let v0 = proj[0];
                for i in 1..proj.len() - 1 {
                    raster_triangle_tex(
                        image, zbuf, v0, proj[i], proj[i + 1],
                        &mt.pixels, tw, th, palette, shade, lightmap.as_ref(), mode,
                    );
                }
            }
            _ => {
                // Flat hashed colour for textureless faces; route through the
                // textured path with a 1x1 colour when a lightmap is present so
                // the per-pixel lightmap factor still applies (matching the world
                // pass exactly).
                let key = ti.map(|t| t.miptex as i64).unwrap_or(face.texinfo as i64);
                let base = hash_color(key);
                match lightmap {
                    Some(lm) => {
                        let mut pal1 = [[0u8; 3]; 256];
                        pal1[0] = [
                            (base[0] * 255.0).clamp(0.0, 255.0) as u8,
                            (base[1] * 255.0).clamp(0.0, 255.0) as u8,
                            (base[2] * 255.0).clamp(0.0, 255.0) as u8,
                        ];
                        let one = [0u8];
                        let v0 = proj[0];
                        for i in 1..proj.len() - 1 {
                            raster_triangle_tex(
                                image, zbuf, v0, proj[i], proj[i + 1],
                                &one, 1, 1, &pal1, shade, Some(&lm), SurfaceMode::Normal,
                            );
                        }
                    }
                    None => {
                        let color = [
                            (base[0] * shade * 255.0).clamp(0.0, 255.0) as u8,
                            (base[1] * shade * 255.0).clamp(0.0, 255.0) as u8,
                            (base[2] * shade * 255.0).clamp(0.0, 255.0) as u8,
                        ];
                        let p0 = Projected { x: proj[0].x, y: proj[0].y, depth: proj[0].vz };
                        for i in 1..proj.len() - 1 {
                            let p1 = Projected { x: proj[i].x, y: proj[i].y, depth: proj[i].vz };
                            let p2 =
                                Projected { x: proj[i + 1].x, y: proj[i + 1].y, depth: proj[i + 1].vz };
                            raster_triangle(image, zbuf, p0, p1, p2, color);
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Alias (MDL) models rendered into the world scene
// ---------------------------------------------------------------------------

/// One alias model placed in the world: the parsed [`Mdl`] plus its world
/// `origin`, `yaw` (degrees, rotation about `+Z`), the animation `frame` to
/// pose, and a flat base `color`.
///
/// Borrows the model so a single parsed `Mdl` (e.g. cached by name) can back
/// many instances without cloning. Rendered by [`draw_alias_model`] /
/// [`render_scene`] sharing the world's z-buffer, so models occlude — and are
/// occluded by — BSP geometry correctly.
///
/// `frame` selects which pose to draw (see [`mdl_frame_verts`]); it is clamped
/// to the model's frame list, so any value is safe and a model with one frame
/// always shows that frame regardless.
pub struct ModelInstance<'a> {
    pub mdl: &'a crate::mdl::Mdl,
    pub origin: Vec3,
    pub yaw: f32,
    pub frame: usize,
    pub color: [u8; 3],
}

/// Resolve the vertices of the pose `frame` for an [`Mdl`], porting the
/// frame-select clamp of Quake's `R_AliasSetupFrame` (`r_alias.c`).
///
/// `frame` is clamped to the model's frame list (`R_AliasSetupFrame` resets an
/// out-of-range frame to 0; we clamp to the last valid index instead, which is
/// equally safe and keeps the highest pose reachable). The selected [`Frame`]
/// resolves to:
///  * `Single(af)` — the single pose's vertices.
///  * `Group { frames, .. }` — the group's *first* sub-pose. Quake cycles a
///    group's poses on a wall-clock timer (`R_AliasSetupFrame` picks by
///    `cl.time` against the group intervals); we have no clock here, so we pick
///    the first sub-pose deterministically.
///
/// Returns `None` only when the model has no frames at all (or, for a group,
/// the group is empty).
fn mdl_frame_verts(mdl: &crate::mdl::Mdl, frame: usize) -> Option<&[crate::mdl::TriVertex]> {
    use crate::mdl::Frame;
    // Clamp `frame` into `[0, len-1]`. `len()` is 0 only for a frameless model,
    // for which `.get()` below returns `None` anyway.
    let last = mdl.frames.len().saturating_sub(1);
    let idx = frame.min(last);
    match mdl.frames.get(idx)? {
        Frame::Single(af) => Some(&af.verts),
        Frame::Group { frames, .. } => frames.first().map(|af| af.verts.as_slice()),
    }
}

/// Decode one MDL vertex into model space:
/// `p[i] = scale[i] * v[i] + scale_origin[i]` (the byte-compressed-vertex
/// reconstruction from Quake's alias renderer).
fn mdl_vertex_model_space(header: &crate::mdl::MdlHeader, tv: &crate::mdl::TriVertex) -> Vec3 {
    [
        header.scale[0] * (tv.v[0] as f32) + header.scale_origin[0],
        header.scale[1] * (tv.v[1] as f32) + header.scale_origin[1],
        header.scale[2] * (tv.v[2] as f32) + header.scale_origin[2],
    ]
}

/// Apply an instance's world transform to a model-space point: rotate about `+Z`
/// by `yaw` (degrees), then translate by `origin`.
fn mdl_model_to_world(p: Vec3, yaw_rad: f64, origin: Vec3) -> Vec3 {
    let (sin_y, cos_y) = (yaw_rad.sin() as f32, yaw_rad.cos() as f32);
    [
        p[0] * cos_y - p[1] * sin_y + origin[0],
        p[0] * sin_y + p[1] * cos_y + origin[1],
        p[2] + origin[2],
    ]
}

/// `ALIAS_ONSEAM` flag (`modelgen.h`): the stvert lies on the texture seam that
/// separates the model skin's front half from its back half.
const ALIAS_ONSEAM: i32 = 0x0020;

/// A usable model skin: its palette-index pixels plus dimensions, borrowed from
/// the [`Mdl`]. Resolved by [`mdl_skin`].
struct ModelSkin<'a> {
    pixels: &'a [u8],
    width: usize,
    height: usize,
}

/// Resolve the texturing skin for an alias model: skin 0's pixels and the
/// header's `skinwidth`/`skinheight`, ported from the `R_AliasDrawModel` skin
/// selection (`r_alias.c`, which uses `pmdl->skinwidth`/`skinheight` and a skin
/// chosen from `paliashdr`'s skin list).
///
/// Returns `None` — so the caller falls back to the flat-colour path for the
/// whole model — when there is no skin, the dimensions are non-positive, the
/// pixel/dimension product overflows, or the pixel buffer is shorter than
/// `skinwidth * skinheight`. A [`Skin::Group`] uses its first frame (we have no
/// wall-clock to cycle skin-group animation, matching how [`mdl_frame_verts`]
/// picks a group's first pose).
fn mdl_skin(mdl: &crate::mdl::Mdl) -> Option<ModelSkin<'_>> {
    use crate::mdl::Skin;
    let pixels: &[u8] = match mdl.skins.first()? {
        Skin::Single(px) => px,
        Skin::Group { frames, .. } => frames.first()?,
    };
    // Dimensions must be strictly positive to index a real grid.
    let width: usize = (mdl.header.skinwidth as i64).try_into().ok()?;
    let height: usize = (mdl.header.skinheight as i64).try_into().ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    let needed = width.checked_mul(height)?;
    if pixels.len() < needed {
        return None;
    }
    Some(ModelSkin {
        pixels,
        width,
        height,
    })
}

/// Compute the skin texel coordinate `(s, t)` for one triangle vertex, porting
/// the onseam/back-face `s`-shift that `GL_MakeAliasModelDisplayLists` /
/// `R_AliasPreparePoints` (and the software `aliastris` setup) apply.
///
/// Quake packs a model's front and back skin halves side by side in one image.
/// A vertex whose stvert carries the `ALIAS_ONSEAM` flag belongs to the seam;
/// when it is referenced by a *back-facing* triangle (`facesfront == 0`) its `s`
/// must be shifted right by `skinwidth / 2` so it samples the back half. Front
/// triangles, and any vertex not on the seam, use the raw `s`. `t` is never
/// shifted.
///
/// Returns texel coordinates as `f32` for [`raster_triangle_tex`]'s
/// perspective-correct interpolation. The result is *not* clamped here; the
/// rasteriser bounds the per-pixel sample.
fn mdl_skin_st(stvert: &crate::mdl::StVert, facesfront: bool, skinwidth: usize) -> (f32, f32) {
    let mut s = stvert.s;
    if (stvert.onseam & ALIAS_ONSEAM) != 0 && !facesfront {
        // skinwidth/2 as i32; skinwidth came from a non-negative header field.
        let half = (skinwidth / 2) as i32;
        s = s.saturating_add(half);
    }
    (s as f32, stvert.t as f32)
}

/// Draw one alias-model instance into `image`/`zbuf`, sharing the world's depth
/// buffer so the model occludes and is occluded by BSP geometry.
///
/// Uses the same camera basis, focal length, projection, and near clip as
/// [`draw_world_textured`]. Each triangle's three vertices (from the instance's
/// posed frame, [`mdl_frame_verts`]) are reconstructed in model space,
/// transformed to world space (yaw about `+Z`, then translate), and projected.
///
/// ## Skin texturing
/// When the model carries a usable skin (resolved by [`mdl_skin`]: skin 0's
/// pixels with positive `skinwidth`/`skinheight` and enough bytes) each triangle
/// is drawn through the perspective-correct textured rasteriser
/// [`raster_triangle_tex`], sampling the palette-indexed skin via `palette`.
/// Per-vertex skin coordinates come from the base ST vertices ([`mdl_skin_st`]),
/// including the `ALIAS_ONSEAM` back-face `s`-shift, and are clamped into the
/// skin so a vertex on the seam never wraps to bleed the opposite half.
///
/// ## Fallback
/// If the model has no usable skin, or a triangle references an out-of-range
/// stvert/vertex, that triangle (or the whole model) is drawn flat with
/// `inst.color` exactly as before, so nothing regresses for un-skinned models.
///
/// Shading is the existing per-triangle Lambert term
/// `max(0.25, dot(normal, light_dir))`, passed as the `shade` argument to the
/// textured rasteriser (and folded into `inst.color` on the flat path). A
/// triangle is skipped whole if any vertex is at/behind the near plane. Every
/// model index goes through `.get()`; malformed data is skipped, never panicked
/// on.
fn draw_alias_model(
    image: &mut Image,
    zbuf: &mut [f32],
    cam: &Camera,
    inst: &ModelInstance,
    w: usize,
    h: usize,
    palette: &[[u8; 3]; 256],
) {
    const NEAR: f32 = 1.0;
    if w == 0 || h == 0 {
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

    // Fixed light direction (same source vector as the world pass).
    let (light_dir, _l) = normalize([0.3, 0.5, 1.0]);
    let yaw_rad = (inst.yaw as f64).to_radians();

    let verts = match mdl_frame_verts(inst.mdl, inst.frame) {
        Some(v) => v,
        None => return, // no frame -> nothing to draw
    };
    let header = &inst.mdl.header;

    // Resolve the model's skin once. `None` => the whole model uses the flat
    // colour path (items without skins, malformed dims, short pixel buffers).
    let skin = mdl_skin(inst.mdl);

    for tri in &inst.mdl.triangles {
        // Resolve the three frame vertices, fully bounds-checked.
        let mut world: [Vec3; 3] = [[0.0; 3]; 3];
        let mut ok = true;
        for (slot, &vi) in tri.vertindex.iter().enumerate() {
            let idx: usize = match usize::try_from(vi) {
                Ok(i) => i,
                Err(_) => {
                    ok = false;
                    break;
                }
            };
            let tv = match verts.get(idx) {
                Some(tv) => tv,
                None => {
                    ok = false;
                    break;
                }
            };
            let p = mdl_vertex_model_space(header, tv);
            // `slot` is 0..3 by the array length; in-range by construction.
            if let Some(w) = world.get_mut(slot) {
                *w = mdl_model_to_world(p, yaw_rad, inst.origin);
            }
        }
        if !ok {
            continue;
        }

        let (a, b, c) = (world[0], world[1], world[2]);

        // World-space flat normal; skip degenerate triangles.
        let (normal, nlen) = normalize(cross(sub(b, a), sub(c, a)));
        if nlen == 0.0 {
            continue;
        }
        let shade = dot(normal, light_dir).clamp(0.25, 1.0);
        let color = [
            (inst.color[0] as f32 * shade).clamp(0.0, 255.0) as u8,
            (inst.color[1] as f32 * shade).clamp(0.0, 255.0) as u8,
            (inst.color[2] as f32 * shade).clamp(0.0, 255.0) as u8,
        ];

        // Per-vertex skin coordinates, if this model has a usable skin AND every
        // vertex of this triangle resolves to a real stvert. The onseam/back-face
        // s-shift is applied, then the coords are clamped into the skin so a seam
        // vertex never wraps into the opposite half (the skin is not tiled).
        let st: Option<[(f32, f32); 3]> = skin.as_ref().and_then(|sk| {
            let facesfront = tri.facesfront != 0;
            let max_s = sk.width.saturating_sub(1) as f32;
            let max_t = sk.height.saturating_sub(1) as f32;
            let mut out = [(0.0f32, 0.0f32); 3];
            for (slot, &vi) in tri.vertindex.iter().enumerate() {
                let idx: usize = usize::try_from(vi).ok()?;
                let sv = inst.mdl.stverts.get(idx)?;
                let (s, t) = mdl_skin_st(sv, facesfront, sk.width);
                // Clamp to [0, w-1]/[0, h-1]: no tiling/wrap for skins.
                let slot_st = out.get_mut(slot)?;
                *slot_st = (s.clamp(0.0, max_s), t.clamp(0.0, max_t));
            }
            Some(out)
        });

        // Project all three; skip the whole triangle if any is at/behind near.
        // `xy[i]` holds (screen-x, screen-y, forward-depth) per vertex.
        let mut xy: [(f32, f32, f32); 3] = [(0.0, 0.0, 0.0); 3];
        let mut clipped = false;
        for (slot, v) in world.iter().enumerate() {
            let rel = sub(*v, cam.pos);
            let vz = dot(rel, forward);
            if vz <= NEAR {
                clipped = true;
                break;
            }
            let vx = dot(rel, right);
            let vy = dot(rel, up);
            if let Some(p) = xy.get_mut(slot) {
                *p = (cx + focal * vx / vz, cy - focal * vy / vz, vz);
            }
        }
        if clipped {
            continue;
        }

        match (&skin, st) {
            (Some(sk), Some(st)) => {
                // Textured: build ProjT vertices and sample the skin through the
                // palette. Models are never lightmapped -> `None` lightmap.
                let mk = |i: usize| ProjT {
                    x: xy[i].0,
                    y: xy[i].1,
                    vz: xy[i].2,
                    s: st[i].0,
                    t: st[i].1,
                };
                raster_triangle_tex(
                    image,
                    zbuf,
                    mk(0),
                    mk(1),
                    mk(2),
                    sk.pixels,
                    sk.width,
                    sk.height,
                    palette,
                    shade,
                    None,
                    SurfaceMode::Normal,
                );
            }
            _ => {
                // Flat fallback (no usable skin, or a triangle's stverts were
                // out of range): draw with the shaded instance colour.
                let p = |i: usize| Projected {
                    x: xy[i].0,
                    y: xy[i].1,
                    depth: xy[i].2,
                };
                raster_triangle(image, zbuf, p(0), p(1), p(2), color);
            }
        }
    }
}

/// One brush submodel placed in the world: which inline model
/// (`bsp.models[model_index]`) to draw and where (`origin`).
///
/// Brush entities (doors, platforms, buttons, triggers with visible brushes)
/// reference an inline submodel through their `model` field `"*N"`, where `N`
/// indexes `bsp.models`. Submodel 0 is the worldspawn (drawn by
/// [`draw_world_textured`]); `N >= 1` are the brush entities, drawn by
/// [`draw_submodel`] at this `origin`. See [`render_scene_ext`].
pub struct BModelInstance {
    pub model_index: usize,
    pub origin: Vec3,
}

/// The player's first-person weapon viewmodel: the parsed weapon [`Mdl`]
/// (`progs/v_shot.mdl` and friends) plus the animation `frame` to pose.
///
/// Unlike [`ModelInstance`], a viewmodel has **no world origin or yaw**: it is
/// anchored to the camera (view space), always drawn in front of the player at
/// the lower-centre of the frame and moving/rotating with the view — Quake's
/// `cl.viewent`, drawn by `R_DrawViewModel`. See [`draw_viewmodel`].
///
/// `frame` selects the pose (clamped by [`mdl_frame_verts`], so any value is
/// safe). The model is borrowed so a cached `Mdl` backs it without cloning.
pub struct Viewmodel<'a> {
    pub mdl: &'a crate::mdl::Mdl,
    pub frame: usize,
}

/// Draw the first-person weapon viewmodel anchored to the camera, on top of all
/// world geometry — a port of Quake's `R_DrawViewModel` (the `cl.viewent`, drawn
/// last at the view origin with the view angles so it never clips into walls).
///
/// ## View anchoring
/// Each model-space vertex `p` (decoded by [`mdl_vertex_model_space`]:
/// `scale*v + scale_origin`) is mapped into the world *relative to the camera*
/// rather than to a fixed world origin:
/// `world_v = cam.pos + forward*(p[0] + FWD) + right*(-p[1] + RIGHT) + up*(p[2] + UP)`.
/// The MDL forward axis (`+X`) maps to the camera's `forward`, the MDL `+Y` to
/// the camera's *left* (hence the `-p[1]` on `right`), and `+Z` to `up`. The
/// fixed `(FWD, RIGHT, UP)` offset nudges the gun forward, slightly right, and
/// down so it sits at the lower-centre of the frame (Quake hangs the gun below
/// and ahead of the eye). Because the basis is the *camera* basis, the gun turns
/// and pitches with the view and never sits at a world position.
///
/// ## Always on top
/// The viewmodel uses its **own** depth buffer (`vz` of its own triangles),
/// cleared fresh here, instead of the shared world z-buffer. So its triangles
/// depth-sort correctly against *each other* (near gun parts occlude far ones)
/// yet always overwrite whatever world/model pixel was there — a wall directly
/// ahead can never hide the gun. The shared world z-buffer is never written, so
/// nothing leaks into the next frame's depth ordering.
///
/// ## Texturing / shading / safety
/// Identical to [`draw_alias_model`]: the model's skin (resolved by [`mdl_skin`])
/// is sampled perspective-correctly through `palette` with the onseam back-face
/// `s`-shift ([`mdl_skin_st`]); a skinless model (or an out-of-range stvert)
/// falls back to a flat shaded grey. Lambert shading uses the same light vector.
/// Every index goes through `.get()`; malformed data is skipped, never panicked
/// on. A triangle is skipped whole if any vertex falls at/behind the near plane.
#[allow(clippy::too_many_arguments)]
fn draw_viewmodel(
    image: &mut Image,
    zbuf: &mut [f32],
    cam: &Camera,
    mdl: &crate::mdl::Mdl,
    frame: usize,
    palette: &[[u8; 3]; 256],
    w: usize,
    h: usize,
) {
    const NEAR: f32 = 1.0;
    // The view-space offset (in MDL/world units) that hangs the gun ahead of,
    // slightly right of, and at the lower-centre of the frame, matching Quake's
    // hand-held pose. These are added in the camera basis below (forward / right
    // / up). `OFS_FORWARD` pushes the *whole* model clear of the near plane —
    // the `v_*` weapon models span roughly model-X in [-15, +22], so a +30 push
    // keeps every vertex in front (no triangle gets near-clipped away) while
    // keeping the gun large; the small +up lifts the (already low, model-Z<0)
    // barrel up into the lower-centre band; `-right` nudges it just right of
    // centre, where Quake draws the player's gun.
    //
    // The placement is in *proportion* resolution-independent: focal length
    // scales with the frame width and the screen centre with its size, so the
    // gun keeps the same lower-centre fraction of the frame at any `w`/`h`.
    const OFS_FORWARD: f32 = 30.0;
    const OFS_RIGHT: f32 = -2.0;
    const OFS_UP: f32 = 2.0;
    if w == 0 || h == 0 {
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

    let (light_dir, _l) = normalize([0.3, 0.5, 1.0]);

    let verts = match mdl_frame_verts(mdl, frame) {
        Some(v) => v,
        None => return, // no frame -> nothing to draw
    };
    let header = &mdl.header;
    let skin = mdl_skin(mdl);

    // The viewmodel owns this depth buffer so it sorts against itself but always
    // overwrites the world (never written here, so it never bleeds across frames).
    let mut local_z = vec![f32::INFINITY; w.saturating_mul(h)];
    let _ = zbuf; // the shared world z-buffer is intentionally left untouched

    for tri in &mdl.triangles {
        // Decode + view-anchor the three frame vertices, fully bounds-checked.
        let mut world: [Vec3; 3] = [[0.0; 3]; 3];
        let mut ok = true;
        for (slot, &vi) in tri.vertindex.iter().enumerate() {
            let idx: usize = match usize::try_from(vi) {
                Ok(i) => i,
                Err(_) => {
                    ok = false;
                    break;
                }
            };
            let tv = match verts.get(idx) {
                Some(tv) => tv,
                None => {
                    ok = false;
                    break;
                }
            };
            let p = mdl_vertex_model_space(header, tv);
            // Anchor to the camera basis: +X -> forward, +Y -> left (so -Y on
            // `right`), +Z -> up; plus the fixed lower-centre offset.
            let fx = p[0] + OFS_FORWARD;
            let rx = -p[1] + OFS_RIGHT;
            let ux = p[2] + OFS_UP;
            if let Some(wv) = world.get_mut(slot) {
                *wv = [
                    cam.pos[0] + forward[0] * fx + right[0] * rx + up[0] * ux,
                    cam.pos[1] + forward[1] * fx + right[1] * rx + up[1] * ux,
                    cam.pos[2] + forward[2] * fx + right[2] * rx + up[2] * ux,
                ];
            }
        }
        if !ok {
            continue;
        }

        let (a, b, c) = (world[0], world[1], world[2]);
        let (normal, nlen) = normalize(cross(sub(b, a), sub(c, a)));
        if nlen == 0.0 {
            continue;
        }
        // The gun faces every which way; light it by |dot| so no facet goes black.
        let shade = dot(normal, light_dir).abs().clamp(0.25, 1.0);
        let flat = [
            (180.0 * shade).clamp(0.0, 255.0) as u8,
            (180.0 * shade).clamp(0.0, 255.0) as u8,
            (180.0 * shade).clamp(0.0, 255.0) as u8,
        ];

        // Per-vertex skin coords (with the onseam back-face s-shift), clamped so
        // a seam vertex never wraps into the opposite half of the skin.
        let st: Option<[(f32, f32); 3]> = skin.as_ref().and_then(|sk| {
            let facesfront = tri.facesfront != 0;
            let max_s = sk.width.saturating_sub(1) as f32;
            let max_t = sk.height.saturating_sub(1) as f32;
            let mut out = [(0.0f32, 0.0f32); 3];
            for (slot, &vi) in tri.vertindex.iter().enumerate() {
                let idx: usize = usize::try_from(vi).ok()?;
                let sv = mdl.stverts.get(idx)?;
                let (s, t) = mdl_skin_st(sv, facesfront, sk.width);
                let slot_st = out.get_mut(slot)?;
                *slot_st = (s.clamp(0.0, max_s), t.clamp(0.0, max_t));
            }
            Some(out)
        });

        // Project all three; skip the whole triangle if any is at/behind near.
        let mut xy: [(f32, f32, f32); 3] = [(0.0, 0.0, 0.0); 3];
        let mut clipped = false;
        for (slot, v) in world.iter().enumerate() {
            let rel = sub(*v, cam.pos);
            let vz = dot(rel, forward);
            if vz <= NEAR {
                clipped = true;
                break;
            }
            let vx = dot(rel, right);
            let vy = dot(rel, up);
            if let Some(p) = xy.get_mut(slot) {
                *p = (cx + focal * vx / vz, cy - focal * vy / vz, vz);
            }
        }
        if clipped {
            continue;
        }

        match (&skin, st) {
            (Some(sk), Some(st)) => {
                let mk = |i: usize| ProjT {
                    x: xy[i].0,
                    y: xy[i].1,
                    vz: xy[i].2,
                    s: st[i].0,
                    t: st[i].1,
                };
                raster_triangle_tex(
                    image, &mut local_z, mk(0), mk(1), mk(2),
                    sk.pixels, sk.width, sk.height, palette, shade, None, SurfaceMode::Normal,
                );
            }
            _ => {
                let p = |i: usize| Projected { x: xy[i].0, y: xy[i].1, depth: xy[i].2 };
                raster_triangle(image, &mut local_z, p(0), p(1), p(2), flat);
            }
        }
    }
}

/// Render `bsp` with its real miptextures (as [`render_bsp_textured`]) and then
/// draw each alias-model `instances` entry into the same image, sharing one
/// z-buffer so models and world occlude one another correctly.
///
/// A thin wrapper over [`render_scene_ext`] with no brush submodels, no
/// viewmodel, and a static (time 0) world; kept as a stable entry point.
///
/// At `time == 0` the animated special surfaces (liquids / sky) show their
/// texture but do not advance; pass an advancing game time through
/// [`render_scene_ext`] to make water ripple and sky scroll.
pub fn render_scene(
    bsp: &Bsp,
    cam: &Camera,
    w: usize,
    h: usize,
    palette: &[[u8; 3]; 256],
    instances: &[ModelInstance],
) -> Image {
    render_scene_ext(
        bsp,
        cam,
        w,
        h,
        palette,
        instances,
        &[],
        None,
        0.0,
        &[],
        &[],
        &NEUTRAL_LIGHTSTYLE_SCALES,
    )
}

/// Render the full scene: the textured world, then each brush submodel
/// (`bmodels`), then each alias model (`models`), and finally the optional
/// first-person `viewmodel` — all sharing one z-buffer so every world/model
/// piece occludes (and is occluded by) the others correctly.
///
/// Brush submodels are drawn *before* alias models, matching `render_scene`'s
/// world-then-models ordering; correctness does not depend on the order because
/// the shared depth buffer resolves visibility per pixel. Passing an empty
/// `bmodels` slice and `None` `viewmodel` reproduces [`render_scene`] exactly.
///
/// The `viewmodel`, when present, is drawn **last and on top** of everything:
/// it is anchored to the camera (Quake's `cl.viewent`) and uses its own depth
/// buffer ([`draw_viewmodel`]), so a wall directly ahead can never hide the gun
/// and the shared world depth buffer is left untouched.
///
/// `time` is the game/server time in seconds, used to animate the special
/// surfaces: liquid faces (miptex name `*…`) get the Quake turbulent SIN warp
/// and sky faces (miptex name `sky…`) get the two-layer scroll. An advancing
/// `time` makes water ripple and sky drift; `time == 0` renders them static
/// (still textured, just not animated). The turbulent sine table is built once
/// per call (a plain `[f32; 256]`, no global state) and shared with the world
/// and brush-submodel passes. Walls, alias models, and the viewmodel ignore
/// `time` entirely.
///
/// ## Particles
/// `particles` is the live set of engine particles (Quake's `particle()`
/// builtin effect: explosions, spawns, blood), each a `(world_pos, palette
/// index)` pair. They are drawn **after** the world / submodels / alias models
/// but **before** the camera-anchored viewmodel, sharing the same internal
/// z-buffer — so a particle behind a wall is correctly hidden, while the gun
/// still draws on top of everything (it has its own depth buffer). Passing an
/// empty `particles` slice draws no particles and leaves the image identical to
/// the pre-particle behaviour, which is why [`render_scene`] and every prior
/// caller can pass `&[]`.
///
/// DESIGN NOTE: the particle slice is a trailing parameter on `render_scene_ext`
/// (option (b) of the task) rather than a separate `draw_particles`-after-render
/// entry point. `render_scene_ext` returns only the `Image`, not its z-buffer,
/// so a standalone post-pass could not depth-test against the world; threading
/// the slice through here lets the particles share the buffer that already
/// exists. [`draw_particles`] is still exposed as a standalone `pub fn` for
/// direct testing of the projection + z-test against a caller-owned buffer.
///
/// ## Dynamic lights
/// `dlights` is the live set of [`crate::dlight::DynamicLight`]s (explosions,
/// muzzle flashes, `EF_*` light effects). They are folded into each lightmapped
/// world (and brush-submodel) face per `R_AddDynamicLights`: a light that
/// reaches a face brightens its luxels, raising the per-pixel lightmap factor
/// near the impact point. Sky and liquid faces are `TEX_SPECIAL` (fullbright, no
/// lightmap) and are never dlit, matching the C. Passing an **empty** `dlights`
/// slice leaves every face borrowing its static lightmap bytes, so the output is
/// byte-identical to the pre-dlight renderer — which is why [`render_scene`] and
/// the demo tests pass `&[]`.
///
/// ## Animated light styles
/// `light_styles` is the per-style brightness scale (`[f32; 64]`, `1.0` ==
/// normal), produced by [`crate::server::Server::lightstyle_scales`] from the
/// map's flickering/pulsing patterns (`R_AnimateLight`). Each lightmapped face
/// selects up to four styles via its `styles[0..3]` slots; the lightmap is the
/// sum of each style's baked luxel block scaled by its `light_styles` value
/// (`R_BuildLightMap`), computed BEFORE dynamic lights are added. Passing the
/// neutral [`NEUTRAL_LIGHTSTYLE_SCALES`] (all `1.0`) reproduces the static
/// style-0 lightmap byte-for-byte, which is what [`render_scene`] does — so the
/// demo tests are unchanged. The animated front-ends pass the live scales each
/// frame to make torches flicker and lights pulse.
#[allow(clippy::too_many_arguments)]
pub fn render_scene_ext(
    bsp: &Bsp,
    cam: &Camera,
    w: usize,
    h: usize,
    palette: &[[u8; 3]; 256],
    models: &[ModelInstance],
    bmodels: &[BModelInstance],
    viewmodel: Option<Viewmodel>,
    time: f32,
    particles: &[(Vec3, u8)],
    dlights: &[crate::dlight::DynamicLight],
    light_styles: &[f32; LIGHTSTYLES],
) -> Image {
    let mut image = Image::new(w, h, [10, 10, 14]);
    if w == 0 || h == 0 {
        return image;
    }
    let mut zbuf = vec![f32::INFINITY; w.saturating_mul(h)];
    // The turbulent SIN table for liquid warp, built once and shared by the
    // world + brush-submodel passes (sky needs no table).
    let turb = TurbTable::new();
    draw_world_textured(&mut image, &mut zbuf, bsp, cam, palette, &turb, time, light_styles, dlights);
    for bm in bmodels {
        draw_submodel(&mut image, &mut zbuf, bsp, cam, palette, bm.model_index, bm.origin, &turb, time, light_styles, dlights);
    }
    for inst in models {
        draw_alias_model(&mut image, &mut zbuf, cam, inst, w, h, palette);
    }
    // Particles draw after the world/models, z-tested against the same buffer so
    // walls occlude them, but before the viewmodel (which always draws on top).
    draw_particles(&mut image, &mut zbuf, cam, particles, palette, w, h);
    // The weapon viewmodel draws last, on top of the world and every model.
    if let Some(vm) = viewmodel {
        draw_viewmodel(&mut image, &mut zbuf, cam, vm.mdl, vm.frame, palette, w, h);
    }
    image
}

/// Draw a set of engine particles into `image`, z-tested and depth-written
/// against the shared `zbuf`, porting the visible result of Quake's software
/// `R_DrawParticles` (`d_*.c`).
///
/// Each particle is `(world_pos, palette index)`. The projection matches every
/// other pass in this module (and [`render_scene_ext`], whose buffer this shares):
/// `rel = p - cam.pos`; the forward depth `vz = dot(rel, forward)` is the z-test
/// key; a particle at or behind the near plane (`vz <= NEAR`) is skipped; the
/// screen position is `sx = cx + focal*dot(rel,right)/vz`,
/// `sy = cy - focal*dot(rel,up)/vz`.
///
/// A particle is drawn as a small filled square whose half-size ramps with
/// `1/vz` (1..=3 px), mirroring `R_DrawParticles`' pixel-size ramp that keeps a
/// near particle from vanishing to a sub-pixel speck. For every covered pixel
/// the existing z-buffer triangle test is reused: the pixel is written only when
/// `vz < zbuf[idx]` (strictly nearer), and the depth is written so later, nearer
/// geometry can still overdraw it. Off-screen pixels are clipped by the loop
/// bounds; the colour is `palette[color]`.
///
/// SAFETY: `w`/`h` of `0`, non-finite projections, and out-of-range indices are
/// all guarded; the only direct indexing is into the freshly-sized framebuffers,
/// where the index is provably in bounds.
pub fn draw_particles(
    image: &mut Image,
    zbuf: &mut [f32],
    cam: &Camera,
    particles: &[(Vec3, u8)],
    palette: &[[u8; 3]; 256],
    w: usize,
    h: usize,
) {
    const NEAR: f32 = 1.0;
    if w == 0 || h == 0 || particles.is_empty() {
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
        let sy = cy - focal * vy / vz;
        if !(sx.is_finite() && sy.is_finite()) {
            continue;
        }

        // Pixel-size ramp: closer particles get a bigger square so a near
        // particle is not a single sub-pixel speck (R_DrawParticles ramped the
        // on-screen size with 1/z). Two buckets:
        //   vz <  512  -> half 1 (3x3 square)
        //   else       -> half 0 (a single pixel for distant particles)
        let half: i64 = if vz < 512.0 { 1 } else { 0 };

        let rgb = palette[color as usize];

        // Centre pixel + a (2*half+1) square around it, each pixel z-tested.
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
                    if vz < *z {
                        *z = vz;
                        if let Some(dst) = image.rgb.get_mut(idx) {
                            *dst = rgb;
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// demo_room: a small renderable test map
// ---------------------------------------------------------------------------

/// Build a small, fully-renderable [`Bsp`] (version 29): an axis-aligned box
/// room from `(-256,-256,-128)` to `(256,256,128)` with six **inward-facing**
/// quad walls (so a camera at the centre sees them), plus a small interior
/// pillar box for depth. Only the lumps the renderer touches are populated
/// (vertexes, edges, surfedges, faces, planes, texinfo, models); every other
/// lump is left empty/default.
pub fn demo_room() -> Bsp {
    use crate::bsp::{DEdge, DFace, DModel, DPlane, DVertex, TexInfo};

    let mut vertexes: Vec<DVertex> = Vec::new();
    let mut edges: Vec<DEdge> = Vec::new();
    let mut surfedges: Vec<i32> = Vec::new();
    let mut faces: Vec<DFace> = Vec::new();
    let mut planes: Vec<DPlane> = Vec::new();

    // Edge 0 is conventionally unused in Quake; reserve a dummy so real edges
    // start at index 1 and we never reference edge 0.
    edges.push(DEdge { v: [0, 0] });

    // A helper closure can't easily mutate captured Vecs cleanly under borrow
    // rules here, so we inline a small "add a quad" routine via a local fn that
    // takes the working buffers by &mut.
    //
    // For each quad we:
    //   * push its 4 corners as vertexes,
    //   * push 4 edges connecting them in CCW order (as seen from inside),
    //   * push 4 surfedges (positive, forward) referencing those edges,
    //   * push a plane (inward normal) and a face referencing the surfedges.
    //
    // `side` is kept 0 and the plane normal is the inward normal directly, so
    // the renderer's cull (`dot(normal, center-pos) >= 0` => skip) keeps the
    // wall visible from a camera inside the room.

    /// Append one quad given its 4 corners (already in the order that, with the
    /// face plane's `inward` normal, makes the face visible from inside) and the
    /// inward face normal + plane distance. Returns nothing; mutates buffers.
    fn add_quad(
        vertexes: &mut Vec<DVertex>,
        edges: &mut Vec<DEdge>,
        surfedges: &mut Vec<i32>,
        faces: &mut Vec<DFace>,
        planes: &mut Vec<DPlane>,
        corners: [[f32; 3]; 4],
        normal: [f32; 3],
        dist: f32,
        texinfo_index: i16,
        ptype: i32,
    ) {
        let base_vtx = vertexes.len() as u16;
        for c in corners {
            vertexes.push(DVertex { point: c });
        }

        let first_edge = surfedges.len() as i32;
        // Four edges around the quad: (0->1),(1->2),(2->3),(3->0).
        for k in 0..4u16 {
            let a = base_vtx + k;
            let b = base_vtx + ((k + 1) % 4);
            let edge_index = edges.len() as i32;
            edges.push(DEdge { v: [a, b] });
            // Forward surfedge (positive => use edge.v[0] as the start vertex,
            // which traverses the quad in the given order).
            surfedges.push(edge_index);
        }

        let planenum = planes.len() as i16;
        planes.push(DPlane {
            normal,
            dist,
            ptype,
        });

        faces.push(DFace {
            planenum,
            side: 0,
            firstedge: first_edge,
            numedges: 4,
            texinfo: texinfo_index,
            styles: [0, 0, 0, 0],
            lightofs: -1,
        });
    }

    // Room bounds.
    let (lo, hi) = ([-256.0f32, -256.0, -128.0], [256.0f32, 256.0, 128.0]);
    let (x0, y0, z0) = (lo[0], lo[1], lo[2]);
    let (x1, y1, z1) = (hi[0], hi[1], hi[2]);

    // PLANE_* type tags (axial). Matching crate::bsp constants: X=0, Y=1, Z=2.
    const PT_X: i32 = 0;
    const PT_Y: i32 = 1;
    const PT_Z: i32 = 2;

    // The six walls, each with an INWARD normal (pointing toward room centre).
    // Corner order is chosen per face; the renderer does not rely on winding for
    // coverage (it accepts both windings) and uses the plane normal for culling.

    // Floor (z = z0): inward normal +Z.
    add_quad(
        &mut vertexes, &mut edges, &mut surfedges, &mut faces, &mut planes,
        [[x0, y0, z0], [x1, y0, z0], [x1, y1, z0], [x0, y1, z0]],
        [0.0, 0.0, 1.0], z0, 0, PT_Z,
    );
    // Ceiling (z = z1): inward normal -Z.
    add_quad(
        &mut vertexes, &mut edges, &mut surfedges, &mut faces, &mut planes,
        [[x0, y0, z1], [x0, y1, z1], [x1, y1, z1], [x1, y0, z1]],
        [0.0, 0.0, -1.0], -z1, 1, PT_Z,
    );
    // West wall (x = x0): inward normal +X.
    add_quad(
        &mut vertexes, &mut edges, &mut surfedges, &mut faces, &mut planes,
        [[x0, y0, z0], [x0, y0, z1], [x0, y1, z1], [x0, y1, z0]],
        [1.0, 0.0, 0.0], x0, 2, PT_X,
    );
    // East wall (x = x1): inward normal -X.
    add_quad(
        &mut vertexes, &mut edges, &mut surfedges, &mut faces, &mut planes,
        [[x1, y0, z0], [x1, y1, z0], [x1, y1, z1], [x1, y0, z1]],
        [-1.0, 0.0, 0.0], -x1, 3, PT_X,
    );
    // South wall (y = y0): inward normal +Y.
    add_quad(
        &mut vertexes, &mut edges, &mut surfedges, &mut faces, &mut planes,
        [[x0, y0, z0], [x1, y0, z0], [x1, y0, z1], [x0, y0, z1]],
        [0.0, 1.0, 0.0], y0, 4, PT_Y,
    );
    // North wall (y = y1): inward normal -Y.
    add_quad(
        &mut vertexes, &mut edges, &mut surfedges, &mut faces, &mut planes,
        [[x0, y1, z0], [x0, y1, z1], [x1, y1, z1], [x1, y1, z0]],
        [0.0, -1.0, 0.0], -y1, 5, PT_Y,
    );

    // --- Interior pillar: a small box near the centre, OUTWARD-facing so the ---
    // camera sees its outside. Spans a modest column for depth cues.
    let (px0, py0, pz0) = (-32.0f32, -32.0, z0);
    let (px1, py1, pz1) = (32.0f32, 32.0, 64.0);

    // Pillar sides (outward normals). Floor/ceiling of the pillar omitted (the
    // base sits on the floor; the top is small) — four side quads suffice.
    // +X face (x = px1): outward normal +X.
    add_quad(
        &mut vertexes, &mut edges, &mut surfedges, &mut faces, &mut planes,
        [[px1, py0, pz0], [px1, py1, pz0], [px1, py1, pz1], [px1, py0, pz1]],
        [1.0, 0.0, 0.0], px1, 6, PT_X,
    );
    // -X face (x = px0): outward normal -X.
    add_quad(
        &mut vertexes, &mut edges, &mut surfedges, &mut faces, &mut planes,
        [[px0, py0, pz0], [px0, py0, pz1], [px0, py1, pz1], [px0, py1, pz0]],
        [-1.0, 0.0, 0.0], -px0, 6, PT_X,
    );
    // +Y face (y = py1): outward normal +Y.
    add_quad(
        &mut vertexes, &mut edges, &mut surfedges, &mut faces, &mut planes,
        [[px0, py1, pz0], [px0, py1, pz1], [px1, py1, pz1], [px1, py1, pz0]],
        [0.0, 1.0, 0.0], py1, 7, PT_Y,
    );
    // -Y face (y = py0): outward normal -Y.
    add_quad(
        &mut vertexes, &mut edges, &mut surfedges, &mut faces, &mut planes,
        [[px0, py0, pz0], [px1, py0, pz0], [px1, py0, pz1], [px0, py0, pz1]],
        [0.0, -1.0, 0.0], -py0, 7, PT_Y,
    );
    // Pillar top (z = pz1): outward normal +Z.
    add_quad(
        &mut vertexes, &mut edges, &mut surfedges, &mut faces, &mut planes,
        [[px0, py0, pz1], [px1, py0, pz1], [px1, py1, pz1], [px0, py1, pz1]],
        [0.0, 0.0, 1.0], pz1, 8, PT_Z,
    );

    // --- A few texinfo entries with distinct miptex indices for varied hues. ---
    // The S/T vectors are unused by this renderer (no texturing) but kept sane.
    let mk_texinfo = |miptex: i32| TexInfo {
        vecs: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
        miptex,
        flags: 0,
    };
    let texinfo: Vec<TexInfo> = (0..9).map(mk_texinfo).collect();

    // --- Model 0: the worldspawn, covering all faces, with the room bounds. ---
    let models = vec![DModel {
        mins: lo,
        maxs: hi,
        origin: [0.0, 0.0, 0.0],
        headnode: [0, 0, 0, 0],
        visleafs: 0,
        firstface: 0,
        numfaces: faces.len() as i32,
    }];

    Bsp {
        version: 29,
        entities: String::new(),
        planes,
        vertexes,
        edges,
        faces,
        nodes: Vec::new(),
        leafs: Vec::new(),
        clipnodes: Vec::new(),
        texinfo,
        models,
        marksurfaces: Vec::new(),
        surfedges,
        textures: Vec::new(),
        visibility: Vec::new(),
        lighting: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// HUD / status bar (Quake's `sbar.c` `Sbar_Draw`)
// ---------------------------------------------------------------------------
//
// Quake's status bar is a 2-D overlay blitted on top of the finished 3-D
// framebuffer. `sbar.c` authored it for a fixed 320x200 virtual screen: the bar
// occupies the bottom 24 rows, with the `sbar` background pic (320x24) drawn
// across the bottom and the big white `num_*` digits stamped on top of it for
// health, current ammo, and armour. The pics live in `gfx.wad` (a WAD2).
//
// This port keeps the same virtual coordinates Quake uses. The caller hands us a
// [`Hud`] holding a borrow of the parsed `gfx.wad`, the palette, and the three
// integer stats read off the player edict; [`draw_hud_into`] then blits the bar
// scaled to the actual framebuffer width and bottom-anchored, so a 320, 480, or
// 640-wide frame all get a full-width bar.
//
// Integration choice (lowest churn): the HUD is a *separate* `pub fn
// draw_hud_into(image, hud)` the scene callers invoke on the returned `Image`,
// rather than a new parameter on `render_scene_ext`. This leaves the renderer's
// signature — and every existing call site and test — untouched, so
// `render_scene`/`render_scene_ext` draw no HUD and all prior tests stay green.
//
// Faithfulness/safety: every WAD pic is fetched with `wad.qpic(name).ok()`, so a
// missing or malformed pic simply doesn't draw (never panics, never errors out
// the frame). Pixel writes go through bounds-checked `Image::put`-style logic,
// and HUD-pic texels equal to palette index 255 are skipped (Quake's transparent
// colour for the status-bar pics).

/// The transparent palette index in Quake's HUD pics: texels equal to 255 are
/// skipped when blitting (`sbar.c` / `draw.c` treat 255 as see-through).
const HUD_TRANSPARENT: u8 = 255;

/// The virtual screen width Quake's `sbar.c` was authored against. The whole bar
/// is laid out in this 320-wide space, then scaled to the real framebuffer.
const HUD_VIRT_W: f32 = 320.0;

/// The status bar's height in virtual rows (`sbar.c` draws it as the bottom 24
/// rows of the 320x200 virtual screen).
const HUD_BAR_H: f32 = 24.0;

/// The Quake HUD overlay: the parsed `gfx.wad`, the screen palette, and the
/// player stats to display. Built by the caller each frame from the player edict
/// and the loaded `gfx.wad`; consumed by [`draw_hud_into`].
///
/// The `wad`/`palette` borrows carry an explicit lifetime `'a` so the caller can
/// keep one parsed [`Wad2`] alive and lend it per frame without cloning.
pub struct Hud<'a> {
    /// The parsed `gfx.wad`, which holds the `sbar`/`num_*`/`anum_*` pics.
    pub wad: &'a crate::wad::Wad2,
    /// The screen palette (`gfx/palette.lmp`), used to colour the pic texels.
    pub palette: &'a [[u8; 3]; 256],
    /// Current player health, drawn as a big number on the left of the bar.
    pub health: i32,
    /// Current ammo for the active weapon, drawn on the right of the bar.
    pub ammo: i32,
    /// Current armour value, drawn just right of the health number.
    pub armor: i32,
}

/// Blit one `Qpic` at virtual position `(vx, vy)` in 320x200 space, scaled by
/// `scale` to the framebuffer and bottom-anchored (so the 24-px bar sits flush
/// at the bottom of any-height frame).
///
/// `vy_top` is the framebuffer y (in pixels) of virtual row 0 of the bar, i.e.
/// `image.h - HUD_BAR_H * scale`; a pic at virtual `(vx, vy)` lands its top-left
/// at `(vx*scale, vy_top + vy*scale)`. Each destination pixel samples its source
/// texel nearest-neighbour; texels equal to [`HUD_TRANSPARENT`] (255) are left
/// transparent, leaving the underlying 3-D pixel untouched. Every write is
/// clipped to the framebuffer, so a pic that overhangs an edge never panics.
fn blit_qpic(
    image: &mut Image,
    pic: &crate::wad::Qpic,
    vx: f32,
    vy: f32,
    scale: f32,
    vy_top: f32,
    palette: &[[u8; 3]; 256],
) {
    if pic.width <= 0 || pic.height <= 0 || scale <= 0.0 {
        return;
    }
    let pw = pic.width as usize;
    let ph = pic.height as usize;
    // Guard against a truncated/short pixel buffer (never index past it).
    if pic.data.len() < pw.saturating_mul(ph) {
        return;
    }

    // Destination top-left in framebuffer pixels, and the scaled pic extent.
    let dst_x0 = (vx * scale).floor() as i64;
    let dst_y0 = (vy_top + vy * scale).floor() as i64;
    let dst_w = (pw as f32 * scale).round().max(1.0) as i64;
    let dst_h = (ph as f32 * scale).round().max(1.0) as i64;
    let inv_scale = 1.0 / scale;

    for dy in 0..dst_h {
        let py = dst_y0 + dy;
        if py < 0 || py >= image.h as i64 {
            continue;
        }
        // Map this destination row back to a source texel row (nearest).
        let sy = (dy as f32 * inv_scale) as usize;
        if sy >= ph {
            continue;
        }
        for dx in 0..dst_w {
            let px = dst_x0 + dx;
            if px < 0 || px >= image.w as i64 {
                continue;
            }
            let sx = (dx as f32 * inv_scale) as usize;
            if sx >= pw {
                continue;
            }
            let texel = match pic.data.get(sy * pw + sx) {
                Some(&t) => t,
                None => continue,
            };
            if texel == HUD_TRANSPARENT {
                continue; // transparent: leave the 3-D pixel as-is
            }
            image.put(px as i32, py as i32, palette[texel as usize]);
        }
    }
}

/// Draw a right-justified non-negative integer using the big `num_*` digit pics
/// (or the gold `anum_*` pics when `alt` is true), porting `Sbar_DrawNum`.
///
/// `(vx, vy)` is the virtual position of the number's **right edge** at its top;
/// digits are laid out leaving-to-right after right-justifying, exactly like
/// Quake (which walks the string from the right, stepping left by each pic's
/// width). Each digit pic's own width drives the spacing, so proportional digit
/// pics still align. A negative value clamps to 0 (the HUD never shows negative
/// stats); any digit whose pic is missing is simply skipped (no panic).
#[allow(clippy::too_many_arguments)]
fn draw_num(
    image: &mut Image,
    value: i32,
    vx: f32,
    vy: f32,
    scale: f32,
    vy_top: f32,
    wad: &crate::wad::Wad2,
    palette: &[[u8; 3]; 256],
    alt: bool,
) {
    // Render the magnitude; the HUD shows 0 for any negative stat.
    let v = if value < 0 { 0 } else { value };
    // Decompose into decimal digits, most-significant first.
    let mut digits: Vec<u32> = Vec::new();
    let mut n = v as u32;
    if n == 0 {
        digits.push(0);
    } else {
        while n > 0 {
            digits.push(n % 10);
            n /= 10;
        }
        digits.reverse();
    }

    // Walk from the rightmost digit leftward, advancing the pen left by each
    // pic's VIRTUAL width — this right-justifies the number at virtual `vx`.
    // `pen` stays in 320-virtual units the whole time; blit_qpic applies `scale`.
    // (The earlier code mixed virtual `vx` with a pixel `w*scale` step, which
    // mis-placed every digit at scale != 1 — i.e. at the real 640-wide frame.)
    let mut pen = vx;
    for &d in digits.iter().rev() {
        let name = if alt {
            ANUM_NAMES[d as usize]
        } else {
            NUM_NAMES[d as usize]
        };
        if let Ok(pic) = wad.qpic(name) {
            let w = pic.width.max(0) as f32;
            pen -= w;
            blit_qpic(image, &pic, pen, vy, scale, vy_top, palette);
        } else {
            // Missing digit pic: still advance by a default 24-virtual slot so
            // the remaining digits keep their right-justified positions.
            pen -= 24.0;
        }
    }
}

/// The white big-number digit pic names (`num_0`..`num_9`).
const NUM_NAMES: [&str; 10] = [
    "num_0", "num_1", "num_2", "num_3", "num_4", "num_5", "num_6", "num_7", "num_8", "num_9",
];

/// The gold/alternate digit pic names (`anum_0`..`anum_9`), used for ammo.
const ANUM_NAMES: [&str; 10] = [
    "anum_0", "anum_1", "anum_2", "anum_3", "anum_4", "anum_5", "anum_6", "anum_7", "anum_8",
    "anum_9",
];

/// Draw the Quake status bar (HUD) across the bottom of `image`, on top of the
/// finished 3-D frame — a port of `sbar.c`'s `Sbar_Draw`.
///
/// The bar is laid out in Quake's 320x200 virtual space and scaled by
/// `image.w / 320` (nearest-neighbour) so it spans the full framebuffer width,
/// bottom-anchored so the 24-px bar sits flush at the bottom regardless of frame
/// height. Drawing order matches Quake:
///  1. the `sbar` background strip (320x24);
///  2. the health number (big white digits, right-justified near virtual x≈154);
///  3. the armour number (just right of health, near virtual x≈49 — Quake draws
///     armour at the left, but we keep it readable beside health here);
///  4. the current ammo (gold digits, right-justified near virtual x≈248).
///
/// All pics are fetched via `wad.qpic(name).ok()`, so a `gfx.wad` missing the
/// `sbar`/digit pics degrades gracefully (those elements just don't draw) and
/// never panics or errors the frame.
pub fn draw_hud_into(image: &mut Image, hud: &Hud) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    // Scale the 320-wide virtual layout to the real framebuffer width.
    let scale = image.w as f32 / HUD_VIRT_W;
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    // Framebuffer y of virtual row 0 of the bar: the 24-px bar sits flush at the
    // bottom (a fractional row is fine — blit_qpic clips at the edges).
    let vy_top = image.h as f32 - HUD_BAR_H * scale;

    // 1. Background strip (sbar, 320x24) at virtual (0,0) of the bar.
    if let Ok(sbar) = hud.wad.qpic("sbar") {
        blit_qpic(image, &sbar, 0.0, 0.0, scale, vy_top, hud.palette);
    }

    // The big digits are ~24 px tall; Quake stamps them at virtual y=0 of the bar
    // (`Sbar_DrawNum(.., y, ..)` with y measured from the bar top). Health on the
    // left, armour beside it, current ammo on the right — all right-justified.
    //
    // 2. Health: right edge near virtual x=154 (Sbar_Draw draws health at x~136
    //    and the 3-digit field ends a little past it).
    draw_num(image, hud.health, 154.0, 0.0, scale, vy_top, hud.wad, hud.palette, false);
    // 3. Armour: far left. Right edge at virtual x=78 so a full 3-digit value
    //    (200 from red armour = 72 virtual px wide) starts at x>=6 and stays on
    //    screen rather than clipping off the left edge.
    draw_num(image, hud.armor, 78.0, 0.0, scale, vy_top, hud.wad, hud.palette, false);
    // 4. Current ammo: gold digits, right edge near virtual x=248.
    draw_num(image, hud.ammo, 248.0, 0.0, scale, vy_top, hud.wad, hud.palette, true);
}

// ---------------------------------------------------------------------------
// Main menu (a port of menu.c: M_Main_Draw/_Key, M_SinglePlayer_Draw/_Key)
// ---------------------------------------------------------------------------
//
// Quake boots INTO this menu (the id logo over the demo loop). It is drawn in the
// SAME 320x200 virtual space `menu.c` uses, on top of the finished game frame,
// with index-255 transparent blits. Navigation is keyboard-only: up/down move a
// 6-frame animated cursor, Enter selects, Escape backs out (or, on the main
// screen, closes the menu).
//
// Faithfulness: the coordinates here are lifted verbatim from `M_Main_Draw` /
// `M_SinglePlayer_Draw` — qplaque at (16,4), the centered title at y=4, the item
// list at (72,32), the cursor at (54, 32 + cursor*20), cursor frame
// `(int)(host_time*10) % 6`. The item *counts* (`MAIN_ITEMS = 5`,
// `SINGLEPLAYER_ITEMS = 3`) and the cursor wrap come straight from `M_Main_Key` /
// `M_SinglePlayer_Key`. Selecting Single Player -> New Game maps to the C's
// `map start` (here we start `e1m1`, the shareware first level).
//
// Safety: every pic is an `Option<Qpic>` in [`MenuPics`]; a missing pic is simply
// skipped (no panic). All blits clip at the framebuffer edges.

/// The virtual screen width/height the menu is authored against (Quake's fixed
/// 320x200 layout). `M_DrawPic`/`M_DrawTransPic` center this in the real screen
/// via `(vid.width - 320) >> 1`; here [`draw_menu`] scales/centers instead.
const MENU_VIRT_W: f32 = 320.0;
const MENU_VIRT_H: f32 = 200.0;

/// `MAIN_ITEMS` (menu.c): the main menu has 5 entries.
const MAIN_ITEMS: usize = 5;
/// `SINGLEPLAYER_ITEMS` (menu.c): the single-player menu has 3 entries.
const SINGLEPLAYER_ITEMS: usize = 3;

/// The shareware first level New Game starts. The C ran `map start`; `start.bsp`
/// is the hub that drops the player into `e1m1`, but for a one-button New Game we
/// jump straight to `e1m1` (the playable first map present in the shareware pak).
pub const NEW_GAME_MAP: &str = "maps/e1m1.bsp";

/// Which menu screen is showing. Mirrors the relevant `m_state` values from
/// menu.c (`m_main`, `m_singleplayer`); the other states (load/save/options/…)
/// are out of scope for this port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuScreen {
    /// The top-level menu (`m_main`): Single Player / Multiplayer / Options /
    /// Help / Quit.
    Main,
    /// The single-player submenu (`m_singleplayer`): New Game / Load / Save.
    SinglePlayer,
}

impl MenuScreen {
    /// The number of selectable items on this screen (the cursor wraps within it).
    fn item_count(self) -> usize {
        match self {
            MenuScreen::Main => MAIN_ITEMS,
            MenuScreen::SinglePlayer => SINGLEPLAYER_ITEMS,
        }
    }
}

/// What pressing Enter (or the menu closing) asks the host to do. The wasm/tool
/// front-end turns these into engine actions (e.g. [`MenuAction::NewGame`]
/// rebuilds the walk on [`NEW_GAME_MAP`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    /// Nothing to do (the selection only changed the screen, or the item is not
    /// implemented in this port — Multiplayer/Options/Help/Load/Save).
    None,
    /// Start a fresh single-player game on [`NEW_GAME_MAP`] and close the menu.
    NewGame,
    /// Backed out of a submenu to the main screen (Escape on a submenu).
    Back,
    /// The menu just closed (Escape on the main screen, or Quit).
    Closed,
}

/// The keyboard-driven main-menu engine: the visible flag, the current screen,
/// and the cursor index within it. A port of menu.c's `m_state` + the
/// `m_*_cursor` globals, scoped to an instance rather than file-statics.
///
/// The host calls [`open`](Menu::open)/[`close`](Menu::close)/[`toggle`](Menu::toggle)
/// to show/hide it, [`move_cursor`](Menu::move_cursor) on up/down, and
/// [`select`](Menu::select)/[`cancel`](Menu::cancel) on Enter/Escape; the returned
/// [`MenuAction`] tells the host what to do. [`draw_menu`] renders the current
/// state.
#[derive(Debug, Clone)]
pub struct Menu {
    /// Whether the menu is showing (drawn + capturing input). Quake's `key_dest ==
    /// key_menu`.
    pub visible: bool,
    /// The screen currently displayed.
    screen: MenuScreen,
    /// The highlighted item index on the current screen (`0..item_count`).
    cursor: usize,
}

impl Default for Menu {
    fn default() -> Self {
        Menu::new()
    }
}

impl Menu {
    /// A closed menu sitting on the main screen with the cursor on the first item.
    pub fn new() -> Menu {
        Menu {
            visible: false,
            screen: MenuScreen::Main,
            cursor: 0,
        }
    }

    /// The screen currently displayed.
    pub fn screen(&self) -> MenuScreen {
        self.screen
    }

    /// The highlighted item index on the current screen.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Open the menu on the main screen (`M_Menu_Main_f`): show it and reset to the
    /// top-level screen with the cursor on the first item.
    pub fn open(&mut self) {
        self.visible = true;
        self.screen = MenuScreen::Main;
        self.cursor = 0;
    }

    /// Close the menu (`key_dest = key_game`). Leaves the screen/cursor as they
    /// were so a later `open` resets them.
    pub fn close(&mut self) {
        self.visible = false;
    }

    /// Toggle the menu (`M_ToggleMenu_f`): if hidden, open on the main screen; if
    /// showing a submenu, go back to main; if already on the main screen, close.
    /// Returns the resulting [`MenuAction`] (`Closed` when it closed, else `None`).
    pub fn toggle(&mut self) -> MenuAction {
        if !self.visible {
            self.open();
            MenuAction::None
        } else if self.screen != MenuScreen::Main {
            self.screen = MenuScreen::Main;
            self.cursor = 0;
            MenuAction::Back
        } else {
            self.close();
            MenuAction::Closed
        }
    }

    /// Move the cursor by `delta` (down = +1, up = -1), wrapping within the current
    /// screen's item count — exactly the `++/--` wrap in `M_Main_Key` /
    /// `M_SinglePlayer_Key`. `delta` may be any magnitude; it wraps modulo the
    /// item count.
    pub fn move_cursor(&mut self, delta: i32) {
        let n = self.screen.item_count();
        if n == 0 {
            self.cursor = 0;
            return;
        }
        let n_i = n as i32;
        // Wrap into 0..n even for large / negative deltas.
        let next = (self.cursor as i32 + delta).rem_euclid(n_i);
        self.cursor = next as usize;
    }

    /// Activate the highlighted item (Enter / `K_ENTER`).
    ///
    /// * Main > Single Player: switch to the single-player screen, cursor reset
    ///   ([`MenuAction::None`]).
    /// * Main > Multiplayer/Options/Help: unimplemented here ([`MenuAction::None`]).
    /// * Main > Quit: close the menu ([`MenuAction::Closed`]).
    /// * SinglePlayer > New Game: [`MenuAction::NewGame`] and close the menu (the
    ///   host starts [`NEW_GAME_MAP`]).
    /// * SinglePlayer > Load/Save: unimplemented here ([`MenuAction::None`]).
    pub fn select(&mut self) -> MenuAction {
        match self.screen {
            MenuScreen::Main => match self.cursor {
                0 => {
                    // M_Menu_SinglePlayer_f
                    self.screen = MenuScreen::SinglePlayer;
                    self.cursor = 0;
                    MenuAction::None
                }
                // Multiplayer / Options / Help: not ported. Quit closes the menu
                // (the C pops a confirm screen; here Quit just dismisses the menu).
                4 => {
                    self.close();
                    MenuAction::Closed
                }
                _ => MenuAction::None,
            },
            MenuScreen::SinglePlayer => match self.cursor {
                0 => {
                    // New Game: the C runs `map start`; we start e1m1 and close.
                    self.close();
                    self.screen = MenuScreen::Main;
                    self.cursor = 0;
                    MenuAction::NewGame
                }
                // Load / Save: not ported.
                _ => MenuAction::None,
            },
        }
    }

    /// Back out (Escape / `K_ESCAPE`): on a submenu return to the main screen
    /// ([`MenuAction::Back`]); on the main screen close the menu
    /// ([`MenuAction::Closed`]). Calling on a hidden menu is a no-op
    /// ([`MenuAction::None`]).
    pub fn cancel(&mut self) -> MenuAction {
        if !self.visible {
            return MenuAction::None;
        }
        match self.screen {
            MenuScreen::SinglePlayer => {
                // M_SinglePlayer_Key K_ESCAPE -> M_Menu_Main_f
                self.screen = MenuScreen::Main;
                self.cursor = 0;
                MenuAction::Back
            }
            MenuScreen::Main => {
                // M_Main_Key K_ESCAPE -> key_dest = key_game
                self.close();
                MenuAction::Closed
            }
        }
    }
}

/// The menu's pre-loaded picture bundle: the plaque, both titles, both item-list
/// graphics, and the 6-frame animated cursor. Each is an `Option` so a pak
/// missing any one degrades gracefully — [`draw_menu`] skips a `None` pic rather
/// than panicking.
///
/// Built once at boot from the PAK's `.lmp` files via [`crate::wad::Qpic::parse`].
#[derive(Debug, Clone, Default)]
pub struct MenuPics {
    /// `gfx/qplaque.lmp` — the decorative left plaque (drawn at (16,4)).
    pub qplaque: Option<crate::wad::Qpic>,
    /// `gfx/ttl_main.lmp` — the "MAIN" title (centered at y=4 on the main screen).
    pub ttl_main: Option<crate::wad::Qpic>,
    /// `gfx/mainmenu.lmp` — the 5-item main menu list graphic (drawn at (72,32)).
    pub mainmenu: Option<crate::wad::Qpic>,
    /// `gfx/ttl_sgl.lmp` — the single-player title (centered at y=4).
    pub ttl_sgl: Option<crate::wad::Qpic>,
    /// `gfx/sp_menu.lmp` — the 3-item single-player list graphic (drawn at (72,32)).
    pub sp_menu: Option<crate::wad::Qpic>,
    /// `gfx/menudot1.lmp`..`menudot6.lmp` — the 6-frame animated cursor.
    pub menudot: [Option<crate::wad::Qpic>; 6],
}

/// Blit one `Qpic` with its top-left at virtual `(vx, vy)` in [`MENU_VIRT_W`] x
/// [`MENU_VIRT_H`] space, scaled by `scale` and offset by `(ox, oy)` framebuffer
/// pixels (so the virtual canvas can be centered in a wider/taller frame).
/// Index-255 texels are transparent; every write clips at the framebuffer edge.
///
/// This is the top-left-anchored sibling of [`blit_qpic`] (which bottom-anchors
/// the HUD). At `scale = 1.0`, `ox = oy = 0` a virtual `(vx, vy)` lands at the
/// framebuffer pixel `(vx, vy)` — the case the 320x200 wasm framebuffer uses, so
/// the menu coordinates from menu.c are used directly with no transform.
fn blit_qpic_at(
    image: &mut Image,
    pic: &crate::wad::Qpic,
    vx: f32,
    vy: f32,
    scale: f32,
    ox: f32,
    oy: f32,
    palette: &[[u8; 3]; 256],
) {
    if pic.width <= 0 || pic.height <= 0 || scale <= 0.0 {
        return;
    }
    let pw = pic.width as usize;
    let ph = pic.height as usize;
    if pic.data.len() < pw.saturating_mul(ph) {
        return;
    }

    let dst_x0 = (ox + vx * scale).floor() as i64;
    let dst_y0 = (oy + vy * scale).floor() as i64;
    let dst_w = (pw as f32 * scale).round().max(1.0) as i64;
    let dst_h = (ph as f32 * scale).round().max(1.0) as i64;
    let inv_scale = 1.0 / scale;

    for dy in 0..dst_h {
        let py = dst_y0 + dy;
        if py < 0 || py >= image.h as i64 {
            continue;
        }
        let sy = (dy as f32 * inv_scale) as usize;
        if sy >= ph {
            continue;
        }
        for dx in 0..dst_w {
            let px = dst_x0 + dx;
            if px < 0 || px >= image.w as i64 {
                continue;
            }
            let sx = (dx as f32 * inv_scale) as usize;
            if sx >= pw {
                continue;
            }
            let texel = match pic.data.get(sy * pw + sx) {
                Some(&t) => t,
                None => continue,
            };
            if texel == HUD_TRANSPARENT {
                continue;
            }
            image.put(px as i32, py as i32, palette[texel as usize]);
        }
    }
}

/// Draw a string of console characters using the 128x128 `conchars` font atlas, a
/// port of Quake's `Draw_String`/`Draw_Character` (`draw.c`).
///
/// `conchars` is the 16x16 grid of 8x8 glyphs (so byte `c`'s glyph sits at cell
/// `(c % 16, c / 16)`, i.e. source pixel `(8*(c%16), 8*(c/16))`). Each character of
/// `text` is stamped 8 virtual pixels apart starting at virtual `(x, y)` in
/// 320x200 space, scaled by `scale` and offset by `(ox, oy)` framebuffer pixels —
/// the same transform [`blit_qpic_at`] uses, so font text lines up with the menu
/// pics.
///
/// Glyph index 0 (the transparent "space" cell whose texels are palette index 0)
/// and the ASCII space are skipped without drawing. Glyph texels equal to palette
/// index 0 are treated as transparent (the conchars atlas uses 0 for the glyph
/// background). The atlas being too small / a glyph cell falling outside it is a
/// silent skip — never a panic.
///
/// The conchars lump in `gfx.wad` is a raw 128x128 byte block (no QPIC header);
/// callers wrap it as a [`crate::wad::Qpic`] with `width = height = 128` and the
/// 16384 lump bytes as `data`.
pub fn draw_string(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    x: i32,
    y: i32,
    text: &str,
    palette: &[[u8; 3]; 256],
) {
    draw_string_scaled(image, conchars, x as f32, y as f32, text, 1.0, 0.0, 0.0, palette);
}

/// The scaled/offset core of [`draw_string`]; the menu draw uses this to place
/// labels in the same scaled+centered virtual space as the pics.
#[allow(clippy::too_many_arguments)]
fn draw_string_scaled(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    vx: f32,
    vy: f32,
    text: &str,
    scale: f32,
    ox: f32,
    oy: f32,
    palette: &[[u8; 3]; 256],
) {
    if conchars.width <= 0 || conchars.height <= 0 || scale <= 0.0 {
        return;
    }
    let cw = conchars.width as usize;
    if conchars.data.len() < cw.saturating_mul(conchars.height as usize) {
        return;
    }
    // The atlas is a 16x16 grid; each glyph is (width/16)x(height/16) source px.
    let cell_w = (conchars.width / 16).max(1) as usize;
    let cell_h = (conchars.height / 16).max(1) as usize;

    let mut pen_vx = vx;
    for ch in text.bytes() {
        // Skip the transparent "space" glyphs (byte 0 and ASCII space): they only
        // hold palette-0 texels, so drawing them is a no-op anyway — but skipping
        // is cheaper and matches the menu's M_Print spacing.
        if ch != 0 && ch != b' ' {
            let cell_x = (ch as usize % 16) * cell_w;
            let cell_y = (ch as usize / 16) * cell_h;
            for gy in 0..cell_h {
                let sy = cell_y + gy;
                if sy >= conchars.height as usize {
                    break;
                }
                let py = (oy + (vy + gy as f32) * scale).floor() as i64;
                for gx in 0..cell_w {
                    let sx = cell_x + gx;
                    if sx >= cw {
                        break;
                    }
                    let texel = match conchars.data.get(sy * cw + sx) {
                        Some(&t) => t,
                        None => continue,
                    };
                    // The conchars atlas uses palette index 0 as the glyph's
                    // transparent background; only stamp the lit texels.
                    if texel == 0 {
                        continue;
                    }
                    let px = (ox + (pen_vx + gx as f32) * scale).floor() as i64;
                    // Stamp a scale x scale block so the glyph is solid when
                    // upscaled (nearest-neighbour); at scale 1 this is one pixel.
                    let block = scale.ceil().max(1.0) as i64;
                    for by in 0..block {
                        for bx in 0..block {
                            image.put((px + bx) as i32, (py + by) as i32, palette[texel as usize]);
                        }
                    }
                }
            }
        }
        pen_vx += 8.0; // M_Print advances the pen 8 virtual px per character.
    }
}

/// Draw the main menu (or single-player submenu) over `image`, a port of
/// `M_Main_Draw` / `M_SinglePlayer_Draw`.
///
/// The layout is Quake's fixed 320x200 virtual canvas, scaled to fit `image`
/// (`scale = min(w/320, h/200)`) and centered, so it looks identical on the
/// 320x200 wasm framebuffer (scale 1, no offset) and on the 640x400 PPM the tool
/// writes (scale 2, centered). `time` is the game clock in seconds; the cursor
/// frame is `(time * 10) as usize % 6` (`(int)(host_time*10) % 6`).
///
/// Each pic is fetched from `pics` and skipped if absent (`None`) — a pak missing
/// the menu art still renders the rest without panicking. `conchars`, when
/// present, draws the small version label at the bottom (purely cosmetic; the
/// menu items themselves come from the `mainmenu`/`sp_menu` graphics, exactly as
/// in Quake).
pub fn draw_menu(
    image: &mut Image,
    menu: &Menu,
    pics: &MenuPics,
    conchars: Option<&crate::wad::Qpic>,
    time: f32,
    palette: &[[u8; 3]; 256],
) {
    if !menu.visible || image.w == 0 || image.h == 0 {
        return;
    }
    // Fit the 320x200 canvas into the frame, centered (integer-ish scale keeps
    // the pixel art crisp; we allow any positive scale and center the remainder).
    let sx = image.w as f32 / MENU_VIRT_W;
    let sy = image.h as f32 / MENU_VIRT_H;
    let scale = sx.min(sy);
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    let ox = (image.w as f32 - MENU_VIRT_W * scale) * 0.5;
    let oy = (image.h as f32 - MENU_VIRT_H * scale) * 0.5;

    // The animated cursor frame: (int)(host_time*10) % 6. Guard a non-finite /
    // negative clock so the index stays 0..6.
    let frame = if time.is_finite() && time > 0.0 {
        ((time * 10.0) as usize) % 6
    } else {
        0
    };

    // The plaque is shared by both screens (M_DrawTransPic (16,4)).
    if let Some(p) = &pics.qplaque {
        blit_qpic_at(image, p, 16.0, 4.0, scale, ox, oy, palette);
    }

    // The centered title + the item-list graphic differ per screen.
    let (title, list) = match menu.screen {
        MenuScreen::Main => (&pics.ttl_main, &pics.mainmenu),
        MenuScreen::SinglePlayer => (&pics.ttl_sgl, &pics.sp_menu),
    };
    if let Some(t) = title {
        // M_DrawPic ((320 - p->width)/2, 4, p).
        let tx = (MENU_VIRT_W - t.width.max(0) as f32) * 0.5;
        blit_qpic_at(image, t, tx, 4.0, scale, ox, oy, palette);
    }
    if let Some(l) = list {
        // M_DrawTransPic (72, 32, ...).
        blit_qpic_at(image, l, 72.0, 32.0, scale, ox, oy, palette);
    }

    // The animated cursor at (54, 32 + cursor*20).
    if let Some(dot) = pics.menudot.get(frame).and_then(|d| d.as_ref()) {
        let cy = 32.0 + menu.cursor as f32 * 20.0;
        blit_qpic_at(image, dot, 54.0, cy, scale, ox, oy, palette);
    }

    // A small version label along the bottom (cosmetic; uses draw_string so the
    // conchars font path is exercised faithfully). Quake stamps the version with
    // the +128 "brown" character range; here we draw plain ASCII.
    if let Some(cc) = conchars {
        draw_string_scaled(image, cc, 4.0, MENU_VIRT_H - 12.0, "quake-rs", scale, ox, oy, palette);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_bob_is_zero_at_rest_and_oscillates_when_moving() {
        // Standing still: no bob, at any time.
        for &t in &[0.0, 0.13, 0.5, 1.7, 42.0] {
            assert_eq!(view_bob(0.0, t), 0.0, "rest bob must be 0 at t={t}");
        }
        // Moving: the bob is bounded to [-7, 4] and actually varies over a cycle
        // (it is a sinusoid of the phase), so min and max across a cycle differ.
        let speed = 320.0; // typical run speed
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for i in 0..120 {
            let t = i as f32 * 0.01; // sweep ~2 bob cycles (cl_bobcycle = 0.6)
            let b = view_bob(speed, t);
            assert!(b.is_finite());
            assert!((-7.0..=4.0).contains(&b), "bob {b} out of clamp range");
            lo = lo.min(b);
            hi = hi.max(b);
        }
        assert!(hi - lo > 0.5, "bob should oscillate over a cycle (got {lo}..{hi})");
        // Faster movement bobs at least as hard as slower (monotone in speed at a
        // fixed phase where sin is positive).
        let t = 0.15; // within the bob-up half, sin(cycle) > 0
        assert!(view_bob(320.0, t) > view_bob(80.0, t));
    }

    #[test]
    fn screen_blend_damage_tint_and_apply() {
        // Content shifts: water/slime/lava tint, empty/solid none.
        assert_eq!(content_cshift(crate::bsp::CONTENTS_WATER), Some(([130, 80, 50], 128.0)));
        assert_eq!(content_cshift(crate::bsp::CONTENTS_LAVA), Some(([255, 80, 0], 150.0)));
        assert_eq!(content_cshift(crate::bsp::CONTENTS_EMPTY), None);

        // No shifts => fully transparent.
        let (_c, a0) = combine_cshifts(&[]);
        assert_eq!(a0, 0.0);

        // A red damage shift gives a reddish blend with partial alpha.
        let (c, a) = combine_cshifts(&[([255, 0, 0], 150.0)]);
        assert!(a > 0.0 && a < 1.0, "alpha {a} should be partial");
        assert!(c[0] > c[1] && c[0] > c[2], "blend should be reddish, got {c:?}");

        // apply_blend with alpha 0 is a no-op; with alpha>0 it moves pixels toward
        // the blend colour.
        let mut img = Image { w: 2, h: 1, rgb: vec![[10, 10, 10], [10, 10, 10]] };
        apply_blend(&mut img, [255, 0, 0], 0.0);
        assert_eq!(img.rgb[0], [10, 10, 10], "alpha 0 must not change pixels");
        apply_blend(&mut img, [255, 0, 0], 0.5);
        assert!(img.rgb[0][0] > 100 && img.rgb[0][1] < 10, "red 0.5 blend: {:?}", img.rgb[0]);
    }

    #[test]
    fn palette_parsing() {
        let mut raw = vec![0u8; 768];
        raw[3] = 10;
        raw[4] = 20;
        raw[5] = 30; // palette index 1
        let pal = parse_palette(&raw).expect("768-byte palette");
        assert_eq!(pal[0], [0, 0, 0]);
        assert_eq!(pal[1], [10, 20, 30]);
        assert!(parse_palette(&[0u8; 100]).is_none(), "short palette rejected");
    }

    #[test]
    fn textured_render_falls_back_without_textures() {
        // demo_room has no inline textures, so the textured path must still draw
        // (via the flat fallback) rather than producing an empty frame.
        let bsp = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let img = render_bsp_textured(&bsp, &cam, 160, 120, &pal);
        let bg = [10u8, 10, 14];
        assert!(img.rgb.iter().any(|&p| p != bg), "textured render drew nothing");
    }

    #[test]
    fn demo_room_shape() {
        let bsp = demo_room();
        assert_eq!(bsp.version, 29);
        assert!(
            bsp.faces.len() >= 6,
            "expected at least 6 faces, got {}",
            bsp.faces.len()
        );
        assert!(
            bsp.vertexes.len() >= 8,
            "expected at least 8 vertexes, got {}",
            bsp.vertexes.len()
        );
        // The worldspawn model should cover all faces.
        let m0 = bsp.models.first().expect("model 0 present");
        assert_eq!(m0.numfaces as usize, bsp.faces.len());
        // Every face's surfedges must resolve (sanity: the map is well-formed).
        for f in &bsp.faces {
            assert!(f.numedges >= 3);
            for i in 0..f.numedges as i32 {
                let se = bsp.surfedges[(f.firstedge + i) as usize];
                let ei = if se >= 0 { se as usize } else { (-se) as usize };
                assert!(bsp.edges.get(ei).is_some(), "edge index in range");
            }
        }
    }

    #[test]
    fn image_put_bounds_checking() {
        let mut img = Image::new(4, 3, [0, 0, 0]);
        // In range: sets the pixel.
        img.put(1, 2, [10, 20, 30]);
        assert_eq!(img.rgb[2 * 4 + 1], [10, 20, 30]);

        // Off-screen in every direction: all no-ops, no panic.
        img.put(-1, 0, [1, 1, 1]);
        img.put(0, -1, [1, 1, 1]);
        img.put(4, 0, [1, 1, 1]); // x == w
        img.put(0, 3, [1, 1, 1]); // y == h
        img.put(100, 100, [1, 1, 1]);

        // The only non-background pixel should still be the one we set.
        let nonblack = img.rgb.iter().filter(|p| **p != [0, 0, 0]).count();
        assert_eq!(nonblack, 1);
    }

    #[test]
    fn render_draws_walls() {
        let bsp = demo_room();
        let cam = Camera::looking_at([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], 90.0);
        let img = render_bsp(&bsp, &cam, 160, 120);

        assert_eq!(img.w, 160);
        assert_eq!(img.h, 120);
        assert_eq!(img.rgb.len(), 160 * 120);

        let bg = [12u8, 12, 18];
        let drawn = img.rgb.iter().filter(|p| **p != bg).count();
        // It must actually have rasterised geometry (walls + pillar), not just
        // background. A central interior view fills a large fraction of pixels.
        assert!(
            drawn > 160 * 120 / 4,
            "expected the renderer to fill a meaningful area, only {drawn} pixels drawn"
        );
    }

    #[test]
    fn render_zbuffer_orders_pillar() {
        // The interior pillar (near, at the centre) must occlude the far wall
        // behind it. Look down +X from near the west wall toward the pillar.
        let bsp = demo_room();
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let img = render_bsp(&bsp, &cam, 160, 120);
        let bg = [12u8, 12, 18];
        let drawn = img.rgb.iter().filter(|p| **p != bg).count();
        assert!(drawn > 0, "z-buffered render produced an image");

        // At least two distinct surface colours should appear (pillar vs walls),
        // proving the z-buffer let nearer geometry win over farther geometry.
        let mut colors: Vec<[u8; 3]> = img.rgb.iter().filter(|p| **p != bg).copied().collect();
        colors.sort();
        colors.dedup();
        assert!(
            colors.len() >= 2,
            "expected multiple surface hues, found {}",
            colors.len()
        );
    }

    #[test]
    fn render_empty_image_is_safe() {
        // Zero-size renders must not panic and return an empty buffer.
        let bsp = demo_room();
        let cam = Camera::looking_at([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], 90.0);
        let img = render_bsp(&bsp, &cam, 0, 0);
        assert_eq!(img.rgb.len(), 0);
    }

    #[test]
    fn render_tolerates_malformed_faces() {
        // Corrupt a face to reference out-of-range edges/vertices and confirm
        // render_bsp skips it without panicking.
        let mut bsp = demo_room();
        if let Some(f) = bsp.faces.get_mut(0) {
            f.firstedge = 1_000_000; // way past surfedges
            f.numedges = 4;
        }
        if let Some(f) = bsp.faces.get_mut(1) {
            f.planenum = 30_000; // past planes
        }
        if let Some(f) = bsp.faces.get_mut(2) {
            f.texinfo = 30_000; // past texinfo
        }
        let cam = Camera::looking_at([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], 90.0);
        // Must not panic.
        let _img = render_bsp(&bsp, &cam, 80, 60);
    }

    /// Build a tiny but valid single-skin single-frame MDL whose frame-0
    /// triangle, after the model->world transform, sits in front of the camera.
    /// Uses a large `scale` so the decoded vertices span a visible extent.
    fn tiny_mdl() -> crate::mdl::Mdl {
        use crate::mdl::{AliasFrame, Frame, Mdl, MdlHeader, Skin, StVert, Triangle, TriVertex};
        let header = MdlHeader {
            ident: i32::from_le_bytes(*b"IDPO"),
            version: 6,
            // 1 unit of v -> 1 world unit; origin shifts to centre the box.
            scale: [1.0, 1.0, 1.0],
            scale_origin: [-16.0, -16.0, -16.0],
            boundingradius: 32.0,
            eyeposition: [0.0, 0.0, 0.0],
            numskins: 1,
            skinwidth: 1,
            skinheight: 1,
            numverts: 3,
            numtris: 1,
            numframes: 1,
            synctype: 0,
            flags: 0,
            size: 1.0,
        };
        let verts = vec![
            TriVertex { v: [0, 0, 0], lightnormalindex: 0 },
            TriVertex { v: [32, 0, 0], lightnormalindex: 0 },
            TriVertex { v: [0, 0, 32], lightnormalindex: 0 },
        ];
        Mdl {
            header,
            skins: vec![Skin::Single(vec![0])],
            stverts: vec![StVert { onseam: 0, s: 0, t: 0 }; 3],
            triangles: vec![Triangle { facesfront: 1, vertindex: [0, 1, 2] }],
            frames: vec![Frame::Single(AliasFrame {
                name: "f0".into(),
                bboxmin: TriVertex { v: [0, 0, 0], lightnormalindex: 0 },
                bboxmax: TriVertex { v: [32, 0, 32], lightnormalindex: 0 },
                verts,
            })],
        }
    }

    #[test]
    fn render_scene_empty_matches_textured() {
        // With no instances, render_scene must be pixel-for-pixel identical to
        // render_bsp_textured — proving the draw_world_textured refactor is
        // behaviour-preserving.
        let bsp = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let baseline = render_bsp_textured(&bsp, &cam, 160, 120, &pal);
        let scene = render_scene(&bsp, &cam, 160, 120, &pal, &[]);
        assert_eq!(scene.w, baseline.w);
        assert_eq!(scene.h, baseline.h);
        assert_eq!(scene.rgb, baseline.rgb, "empty scene must equal textured render");
    }

    #[test]
    fn render_scene_model_changes_pixels() {
        // A model placed in front of the camera must alter some pixels relative
        // to the world-only render.
        let bsp = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        // Look from the west wall toward the centre (down +X).
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let world_only = render_scene(&bsp, &cam, 160, 120, &pal, &[]);

        let mdl = tiny_mdl();
        let inst = ModelInstance {
            mdl: &mdl,
            origin: [-80.0, 0.0, 0.0], // between the camera and the centre
            yaw: 0.0,
            frame: 0,
            color: [255, 32, 32],
        };
        let with_model = render_scene(&bsp, &cam, 160, 120, &pal, std::slice::from_ref(&inst));

        let changed = world_only
            .rgb
            .iter()
            .zip(with_model.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "model in front of camera changed no pixels");
    }

    /// Build a synthetic two-frame single-skin MDL. Frame 0 and frame 1 carry
    /// distinct vertex data so the selected pose is observable.
    fn two_frame_mdl() -> crate::mdl::Mdl {
        use crate::mdl::{AliasFrame, Frame, Mdl, MdlHeader, Skin, StVert, Triangle, TriVertex};
        let header = MdlHeader {
            ident: i32::from_le_bytes(*b"IDPO"),
            version: 6,
            // 1 unit of v -> 1 world unit; origin centres the box (matches the
            // proven-visible geometry of `tiny_mdl`).
            scale: [1.0, 1.0, 1.0],
            scale_origin: [-16.0, -16.0, -16.0],
            boundingradius: 32.0,
            eyeposition: [0.0, 0.0, 0.0],
            numskins: 1,
            skinwidth: 1,
            skinheight: 1,
            numverts: 3,
            numtris: 1,
            numframes: 2,
            synctype: 0,
            flags: 0,
            size: 1.0,
        };
        // Frame 0: the same triangle `tiny_mdl` uses (known to rasterise here).
        let frame0 = AliasFrame {
            name: "pose0".into(),
            bboxmin: TriVertex { v: [0, 0, 0], lightnormalindex: 0 },
            bboxmax: TriVertex { v: [32, 0, 32], lightnormalindex: 0 },
            verts: vec![
                TriVertex { v: [0, 0, 0], lightnormalindex: 0 },
                TriVertex { v: [32, 0, 0], lightnormalindex: 0 },
                TriVertex { v: [0, 0, 32], lightnormalindex: 0 },
            ],
        };
        // Frame 1: a clearly different pose — the triangle spread along +Y so it
        // faces the camera differently and covers a different screen region.
        let frame1 = AliasFrame {
            name: "pose1".into(),
            bboxmin: TriVertex { v: [0, 0, 0], lightnormalindex: 0 },
            bboxmax: TriVertex { v: [255, 255, 255], lightnormalindex: 0 },
            verts: vec![
                TriVertex { v: [255, 0, 255], lightnormalindex: 0 },
                TriVertex { v: [255, 255, 0], lightnormalindex: 0 },
                TriVertex { v: [0, 255, 255], lightnormalindex: 0 },
            ],
        };
        Mdl {
            header,
            skins: vec![Skin::Single(vec![0])],
            stverts: vec![StVert { onseam: 0, s: 0, t: 0 }; 3],
            triangles: vec![Triangle { facesfront: 1, vertindex: [0, 1, 2] }],
            frames: vec![Frame::Single(frame0), Frame::Single(frame1)],
        }
    }

    #[test]
    fn mdl_frame_verts_selects_and_clamps() {
        use crate::mdl::TriVertex;
        let mdl = two_frame_mdl();

        // Frame 0 -> first pose.
        let f0 = mdl_frame_verts(&mdl, 0).expect("frame 0 present");
        assert_eq!(f0.len(), 3);
        assert_eq!(f0[0], TriVertex { v: [0, 0, 0], lightnormalindex: 0 });
        assert_eq!(f0[1], TriVertex { v: [32, 0, 0], lightnormalindex: 0 });

        // Frame 1 -> second, distinct pose.
        let f1 = mdl_frame_verts(&mdl, 1).expect("frame 1 present");
        assert_eq!(f1[0], TriVertex { v: [255, 0, 255], lightnormalindex: 0 });
        assert_eq!(f1[2], TriVertex { v: [0, 255, 255], lightnormalindex: 0 });

        // Out-of-range frame clamps to the last frame (index 1) rather than
        // panicking or returning None.
        let clamped = mdl_frame_verts(&mdl, 999).expect("clamped frame present");
        assert_eq!(clamped, f1, "out-of-range frame should clamp to the last pose");

        // A frameless model yields None (no pose to draw).
        let mut empty = two_frame_mdl();
        empty.frames.clear();
        assert!(mdl_frame_verts(&empty, 0).is_none());

        // A group frame resolves to its first sub-pose deterministically.
        {
            use crate::mdl::{AliasFrame, Frame, TriVertex as TV};
            let mut grouped = two_frame_mdl();
            let sub0 = AliasFrame {
                name: "g0".into(),
                bboxmin: TV { v: [0, 0, 0], lightnormalindex: 0 },
                bboxmax: TV { v: [1, 1, 1], lightnormalindex: 0 },
                verts: vec![
                    TV { v: [7, 0, 0], lightnormalindex: 0 },
                    TV { v: [8, 0, 0], lightnormalindex: 0 },
                    TV { v: [9, 0, 0], lightnormalindex: 0 },
                ],
            };
            let sub1 = AliasFrame {
                name: "g1".into(),
                bboxmin: TV { v: [0, 0, 0], lightnormalindex: 0 },
                bboxmax: TV { v: [1, 1, 1], lightnormalindex: 0 },
                verts: vec![
                    TV { v: [50, 0, 0], lightnormalindex: 0 },
                    TV { v: [51, 0, 0], lightnormalindex: 0 },
                    TV { v: [52, 0, 0], lightnormalindex: 0 },
                ],
            };
            grouped.frames = vec![Frame::Group {
                bboxmin: TV { v: [0, 0, 0], lightnormalindex: 0 },
                bboxmax: TV { v: [1, 1, 1], lightnormalindex: 0 },
                intervals: vec![0.1, 0.2],
                frames: vec![sub0, sub1],
            }];
            let g = mdl_frame_verts(&grouped, 0).expect("group first sub-pose");
            assert_eq!(g[0], TriVertex { v: [7, 0, 0], lightnormalindex: 0 });
        }
    }

    // -- Alias-model skin texturing: skin resolution + onseam texcoord math --

    #[test]
    fn mdl_skin_st_onseam_backface_shift() {
        use crate::mdl::StVert;
        const SKINWIDTH: usize = 100;

        // A vertex NOT on the seam: s is the raw stvert.s on both front and back
        // triangles, t is always the raw stvert.t.
        let plain = StVert { onseam: 0, s: 10, t: 7 };
        assert_eq!(mdl_skin_st(&plain, true, SKINWIDTH), (10.0, 7.0));
        assert_eq!(mdl_skin_st(&plain, false, SKINWIDTH), (10.0, 7.0));

        // A seam vertex (ALIAS_ONSEAM set): on a FRONT triangle s is unshifted;
        // on a BACK triangle s is shifted right by skinwidth/2. The two cases
        // must differ by exactly skinwidth/2; t is unchanged.
        let seam = StVert { onseam: ALIAS_ONSEAM, s: 10, t: 7 };
        let (front_s, front_t) = mdl_skin_st(&seam, true, SKINWIDTH);
        let (back_s, back_t) = mdl_skin_st(&seam, false, SKINWIDTH);
        assert_eq!((front_s, front_t), (10.0, 7.0), "front seam vertex unshifted");
        assert_eq!((back_s, back_t), (60.0, 7.0), "back seam vertex shifted by w/2");
        assert_eq!(back_s - front_s, (SKINWIDTH / 2) as f32);

        // Other bits set in `onseam` but not ALIAS_ONSEAM -> treated as not-seam.
        let other = StVert { onseam: 0x0001, s: 10, t: 7 };
        assert_eq!(mdl_skin_st(&other, false, SKINWIDTH), (10.0, 7.0));
    }

    #[test]
    fn mdl_skin_resolves_single_and_rejects_bad() {
        use crate::mdl::Skin;

        // tiny_mdl has skinwidth=1, skinheight=1 and a 1-byte single skin.
        let mut mdl = tiny_mdl();
        let sk = mdl_skin(&mdl).expect("1x1 single skin resolves");
        assert_eq!((sk.width, sk.height), (1, 1));
        assert_eq!(sk.pixels.len(), 1);

        // A 2x2 single skin with the right number of pixels resolves.
        mdl.header.skinwidth = 2;
        mdl.header.skinheight = 2;
        mdl.skins = vec![Skin::Single(vec![1, 2, 3, 4])];
        let sk = mdl_skin(&mdl).expect("2x2 single skin resolves");
        assert_eq!((sk.width, sk.height), (2, 2));
        assert_eq!(sk.pixels, &[1, 2, 3, 4]);

        // Too few pixels for the claimed dimensions -> None (flat fallback).
        mdl.skins = vec![Skin::Single(vec![1, 2, 3])];
        assert!(mdl_skin(&mdl).is_none(), "short pixel buffer must be rejected");

        // Non-positive dimensions -> None.
        let mut zero = tiny_mdl();
        zero.header.skinwidth = 0;
        assert!(mdl_skin(&zero).is_none(), "zero skinwidth must be rejected");

        // No skins at all -> None.
        let mut noskin = tiny_mdl();
        noskin.skins.clear();
        assert!(mdl_skin(&noskin).is_none(), "skinless model must be rejected");

        // A skin GROUP resolves via its first frame.
        let mut grouped = tiny_mdl();
        grouped.header.skinwidth = 2;
        grouped.header.skinheight = 1;
        grouped.skins = vec![Skin::Group {
            intervals: vec![0.1, 0.2],
            frames: vec![vec![5, 6], vec![7, 8]],
        }];
        let sk = mdl_skin(&grouped).expect("skin group resolves via first frame");
        assert_eq!((sk.width, sk.height), (2, 1));
        assert_eq!(sk.pixels, &[5, 6], "group uses its first frame deterministically");
    }

    /// A `tiny_mdl` variant carrying a 2x2 skin whose four texels map to four
    /// distinct, vivid palette colours, with stverts spread across the skin so
    /// the rasteriser actually samples more than one texel.
    fn skinned_mdl() -> crate::mdl::Mdl {
        use crate::mdl::{Skin, StVert};
        let mut mdl = tiny_mdl();
        mdl.header.skinwidth = 2;
        mdl.header.skinheight = 2;
        // Texel indices 1,2,3 (palette entries set vividly in the test).
        mdl.skins = vec![Skin::Single(vec![1, 2, 3, 1])];
        // Spread the three triangle vertices to three corners of the 2x2 skin.
        mdl.stverts = vec![
            StVert { onseam: 0, s: 0, t: 0 },
            StVert { onseam: 0, s: 1, t: 0 },
            StVert { onseam: 0, s: 0, t: 1 },
        ];
        mdl
    }

    #[test]
    fn render_scene_skinned_differs_from_flat() {
        // A model with a real skin must render differently from the same model
        // forced down the flat-colour path (skin removed), proving the skin is
        // sampled rather than ignored.
        let bsp = demo_room();

        // A palette where the skin's texel indices map to vivid, distinct colours
        // unlikely to coincide with the flat instance colour after shading.
        let mut pal = [[0u8; 3]; 256];
        pal[1] = [255, 0, 0];
        pal[2] = [0, 255, 0];
        pal[3] = [0, 0, 255];

        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);

        // Skinned model.
        let skinned = skinned_mdl();
        let inst_skin = ModelInstance {
            mdl: &skinned,
            origin: [-80.0, 0.0, 0.0],
            yaw: 0.0,
            frame: 0,
            color: [255, 32, 32],
        };
        let img_skin = render_scene(&bsp, &cam, 160, 120, &pal, std::slice::from_ref(&inst_skin));

        // Same model/instance but with the skin stripped -> flat fallback path.
        let mut flat = skinned_mdl();
        flat.skins.clear();
        let inst_flat = ModelInstance {
            mdl: &flat,
            origin: [-80.0, 0.0, 0.0],
            yaw: 0.0,
            frame: 0,
            color: [255, 32, 32],
        };
        let img_flat = render_scene(&bsp, &cam, 160, 120, &pal, std::slice::from_ref(&inst_flat));

        let changed = img_skin
            .rgb
            .iter()
            .zip(img_flat.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "skinned model should differ from flat-colour model");

        // The skinned render must actually show one of the skin's palette colours
        // somewhere (red/green/blue), confirming the skin pixels are sampled.
        let shows_skin_color = img_skin.rgb.iter().any(|&p| {
            // After Lambert shading the channel scales down, but a pure-channel
            // skin colour stays a pure channel (the other two channels stay 0).
            (p[0] > 0 && p[1] == 0 && p[2] == 0)
                || (p[1] > 0 && p[0] == 0 && p[2] == 0)
                || (p[2] > 0 && p[0] == 0 && p[1] == 0)
        });
        assert!(shows_skin_color, "expected a sampled skin colour in the skinned render");
    }

    #[test]
    fn render_scene_skinless_model_still_draws() {
        // A model without a skin must still draw (flat fallback), unchanged from
        // the pre-skin behaviour: placing it in front of the camera alters pixels.
        let bsp = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let world_only = render_scene(&bsp, &cam, 160, 120, &pal, &[]);

        let mut mdl = tiny_mdl();
        mdl.skins.clear(); // no usable skin -> flat path
        let inst = ModelInstance {
            mdl: &mdl,
            origin: [-80.0, 0.0, 0.0],
            yaw: 0.0,
            frame: 0,
            color: [255, 32, 32],
        };
        let with_model = render_scene(&bsp, &cam, 160, 120, &pal, std::slice::from_ref(&inst));
        let changed = world_only
            .rgb
            .iter()
            .zip(with_model.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "skinless model must still draw via the flat fallback");
    }

    #[test]
    fn render_scene_frame_selection_changes_pixels() {
        // Two instances differing only in `frame` must render differently when
        // the two frames carry distinct geometry.
        let bsp = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let mdl = two_frame_mdl();

        let inst0 = ModelInstance {
            mdl: &mdl,
            origin: [-80.0, 0.0, 0.0],
            yaw: 0.0,
            frame: 0,
            color: [255, 32, 32],
        };
        let inst1 = ModelInstance {
            mdl: &mdl,
            origin: [-80.0, 0.0, 0.0],
            yaw: 0.0,
            frame: 1,
            color: [255, 32, 32],
        };
        let img0 = render_scene(&bsp, &cam, 160, 120, &pal, std::slice::from_ref(&inst0));
        let img1 = render_scene(&bsp, &cam, 160, 120, &pal, std::slice::from_ref(&inst1));
        let changed = img0
            .rgb
            .iter()
            .zip(img1.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "different frames should produce different images");
    }

    #[test]
    fn ppm_header_sanity() {
        // Write a tiny image to a temp PPM, check the P6 header, then remove it.
        let img = Image::new(2, 1, [255, 0, 0]);
        let mut path = std::env::temp_dir();
        path.push(format!("quake_rs_render_test_{}.ppm", std::process::id()));
        let path_str = path.to_string_lossy().into_owned();

        img.write_ppm(&path_str).expect("write ppm");
        let bytes = std::fs::read(&path_str).expect("read ppm back");

        // Header: "P6\n2 1\n255\n" followed by 2*1*3 = 6 bytes of pixel data.
        let header = b"P6\n2 1\n255\n";
        assert!(
            bytes.starts_with(header),
            "PPM header mismatch: {:?}",
            &bytes[..header.len().min(bytes.len())]
        );
        assert_eq!(bytes.len(), header.len() + 6);
        // First pixel is red.
        assert_eq!(&bytes[header.len()..header.len() + 3], &[255, 0, 0]);

        // Clean up the temp file.
        let _ = std::fs::remove_file(&path_str);
    }

    // -- Lightmap: surface extents / luxel math ----------------------------

    /// An axis-aligned texinfo: s along world X, t along world Y, no offset.
    fn axis_texinfo() -> crate::bsp::TexInfo {
        crate::bsp::TexInfo {
            vecs: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            miptex: 0,
            flags: 0,
        }
    }

    #[test]
    fn surface_extents_basic() {
        let ti = axis_texinfo();
        // A 32x32 square: surface s in [0,32], t in [0,32].
        let poly = [
            [0.0, 0.0, 0.0],
            [32.0, 0.0, 0.0],
            [32.0, 32.0, 0.0],
            [0.0, 32.0, 0.0],
        ];
        let (texmins, extent) = surface_extents(&ti, &poly).unwrap();
        // bmin = floor(0/16) = 0, bmax = ceil(32/16) = 2 -> texmin 0, extent 32.
        assert_eq!(texmins, [0, 0]);
        assert_eq!(extent, [32, 32]);
        // Luxel grid is extent/16 + 1 = 3x3.
        assert_eq!((extent[0] / 16 + 1, extent[1] / 16 + 1), (3, 3));
    }

    #[test]
    fn surface_extents_non_aligned() {
        let ti = axis_texinfo();
        // s in [3, 20]  -> bmin=floor(3/16)=0,  bmax=ceil(20/16)=2 -> mins 0,  ext 32.
        // t in [-5, 5]  -> bmin=floor(-5/16)=-1, bmax=ceil(5/16)=1 -> mins -16, ext 32.
        let poly = [
            [3.0, -5.0, 0.0],
            [20.0, -5.0, 0.0],
            [20.0, 5.0, 0.0],
            [3.0, 5.0, 0.0],
        ];
        let (texmins, extent) = surface_extents(&ti, &poly).unwrap();
        assert_eq!(texmins, [0, -16]);
        assert_eq!(extent, [32, 32]);
    }

    #[test]
    fn surface_extents_rejects_degenerate() {
        let ti = axis_texinfo();
        let poly = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]];
        assert!(surface_extents(&ti, &poly).is_none());
    }

    #[test]
    fn lightmap_factor_bilinear_and_clamp() {
        // 2x2 luxel grid, texmins at origin so luxel coord = s/16.
        let samples = [0u8, 255u8, 255u8, 0u8]; // checker
        let lm = LightMap {
            luxels: Luxels::Static(&samples),
            lmw: 2,
            lmh: 2,
            texmins: [0.0, 0.0],
        };
        // At (0,0): luxel (0,0) = 0 -> factor 0.
        assert!((lm.factor_at(0.0, 0.0) - 0.0).abs() < 1e-6);
        // At (16,0): luxel (1,0) = 255 -> factor 2.0 (overbright).
        assert!((lm.factor_at(16.0, 0.0) - 2.0).abs() < 1e-6);
        // Centre (8,8) averages all four luxels -> 127.5/255*2.
        let mid = lm.factor_at(8.0, 8.0);
        assert!((mid - (127.5 / 255.0 * 2.0)).abs() < 1e-3);
        // Coordinates far past the edge clamp to the last luxel (no panic).
        assert!((lm.factor_at(10_000.0, 10_000.0) - 0.0).abs() < 1e-6); // luxel (1,1)=0
        // Negative coords clamp to luxel 0.
        assert!((lm.factor_at(-500.0, -500.0) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn lightmap_oob_sample_is_fullbright() {
        // Claims 2x2 but only 1 byte present: out-of-slice luxels read as
        // fullbright (255 -> factor 2.0) rather than panicking.
        let samples = [0u8];
        let lm = LightMap {
            luxels: Luxels::Static(&samples),
            lmw: 2,
            lmh: 2,
            texmins: [0.0, 0.0],
        };
        // luxel (1,1) is index 3, out of the 1-byte slice -> fullbright.
        assert!((lm.factor_at(16.0, 16.0) - 2.0).abs() < 1e-6);
    }

    /// Build a one-face BSP with the given lighting lump, lightofs, and flags,
    /// plus a 32x32 (=> 3x3 luxel) world polygon.
    fn one_face_bsp(
        lighting: Vec<u8>,
        lightofs: i32,
        flags: i32,
    ) -> (Bsp, crate::bsp::DFace, Vec<Vec3>) {
        let mut bsp = demo_room();
        // Replace texinfo[0] with an axis-aligned one and clear textures so the
        // texinfo lookup in face_lightmap resolves predictably.
        bsp.texinfo = vec![crate::bsp::TexInfo {
            vecs: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            miptex: 0,
            flags,
        }];
        bsp.lighting = lighting;
        let face = crate::bsp::DFace {
            planenum: 0,
            side: 0,
            firstedge: 0,
            numedges: 4,
            texinfo: 0,
            styles: [0, 0, 0, 0],
            lightofs,
        };
        let poly = vec![
            [0.0, 0.0, 0.0],
            [32.0, 0.0, 0.0],
            [32.0, 32.0, 0.0],
            [0.0, 32.0, 0.0],
        ];
        (bsp, face, poly)
    }

    #[test]
    fn face_lightmap_present() {
        let (bsp, face, poly) = one_face_bsp(vec![200u8; 9], 0, 0);
        let lm = face_lightmap(&bsp, &face, &poly).expect("lightmap should be present");
        assert_eq!((lm.lmw, lm.lmh), (3, 3));
        match &lm.luxels {
            Luxels::Static(s) => assert_eq!(s.len(), 9),
            Luxels::Owned(_) => panic!("no dlights -> static borrow expected"),
        }
        assert_eq!(lm.texmins, [0.0, 0.0]);
        // Every luxel 200 -> factor 200/255*2 ~= 1.568.
        assert!((lm.factor_at(0.0, 0.0) - (200.0 / 255.0 * 2.0)).abs() < 1e-6);
    }

    #[test]
    fn face_lightmap_fullbright_cases() {
        // No lighting lump -> fullbright.
        let (bsp, face, poly) = one_face_bsp(Vec::new(), 0, 0);
        assert!(face_lightmap(&bsp, &face, &poly).is_none());

        // lightofs < 0 -> fullbright.
        let (bsp, face, poly) = one_face_bsp(vec![200u8; 9], -1, 0);
        assert!(face_lightmap(&bsp, &face, &poly).is_none());

        // TEX_SPECIAL (sky/liquid) -> fullbright.
        let (bsp, face, poly) = one_face_bsp(vec![200u8; 9], 0, crate::bsp::TEX_SPECIAL);
        assert!(face_lightmap(&bsp, &face, &poly).is_none());

        // Grid needs 9 luxels but only 4 available -> fullbright (no overrun).
        let (bsp, face, poly) = one_face_bsp(vec![200u8; 4], 0, 0);
        assert!(face_lightmap(&bsp, &face, &poly).is_none());

        // lightofs near the end leaves too few bytes -> fullbright.
        let (bsp, face, poly) = one_face_bsp(vec![200u8; 9], 5, 0);
        assert!(face_lightmap(&bsp, &face, &poly).is_none());
    }

    // -- Dynamic lights (R_AddDynamicLights) -------------------------------

    use crate::dlight::DynamicLight;

    /// `one_face_bsp` with plane 0 forced to the z=0 surface plane (`normal
    /// [0,0,1]`, `dist 0`) so a light's distance/impact math is predictable: the
    /// 3x3-luxel face spans surface `(s,t)` in `0..=32` (texmins 0, axis-aligned
    /// vecs), so luxel `(i,j)` lives at world `(16i, 16j, 0)`.
    fn one_face_bsp_zplane(luxel: u8) -> (Bsp, crate::bsp::DFace, Vec<Vec3>) {
        let (mut bsp, face, poly) = one_face_bsp(vec![luxel; 9], 0, 0);
        bsp.planes[0] = crate::bsp::DPlane {
            normal: [0.0, 0.0, 1.0],
            dist: 0.0,
            ptype: 0,
        };
        (bsp, face, poly)
    }

    #[test]
    fn dynamic_light_brightens_near_luxel_only() {
        // Static luxels all 100; factor there is 100/255*2 ~= 0.784.
        let (bsp, face, poly) = one_face_bsp_zplane(100);
        let static_factor = 100.0 / 255.0 * 2.0;

        // A bright light 16 units above luxel (0,0) (world [0,0,16]); small radius
        // so it lights the near corner but not the far one.
        let dl = DynamicLight::new([0.0, 0.0, 16.0], 60.0, 10.0, 0.0, 0.0, 0);
        let lm = face_lightmap_dyn(&bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&dl))
            .expect("lightmap present");
        // It must have switched to the owned augmented buffer.
        assert!(matches!(lm.luxels, Luxels::Owned(_)), "a reaching light must own the buffer");

        // Near luxel (s=0,t=0): dist=16, rad=60-16=44, dist2=0 -> +44 -> 144.
        // factor 144/255*2 ~= 1.13, brighter than the static 0.784.
        let near = lm.factor_at(0.0, 0.0);
        assert!(near > static_factor + 0.2, "near luxel should brighten: {near} vs {static_factor}");

        // Far luxel (s=32,t=32) = world (32,32,0): sd=td=32, dist2=48 >= rad(44)
        // -> no add, stays at the static value.
        let far = lm.factor_at(32.0, 32.0);
        assert!((far - static_factor).abs() < 1e-4, "far luxel must be unchanged: {far} vs {static_factor}");
    }

    #[test]
    fn empty_dlights_keeps_static_borrow_and_factor() {
        // With no dlights the lightmap must borrow the static bytes and sample
        // exactly the pre-dlight factor (byte-identical behaviour).
        let (bsp, face, poly) = one_face_bsp_zplane(150);
        let with_none = face_lightmap_dyn(&bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, &[]).expect("present");
        assert!(matches!(with_none.luxels, Luxels::Static(_)), "empty slice keeps static borrow");

        let baseline = face_lightmap(&bsp, &face, &poly).expect("present");
        // Same sampled factor at several points.
        for &(s, t) in &[(0.0f32, 0.0f32), (8.0, 8.0), (16.0, 16.0), (32.0, 32.0)] {
            assert!(
                (with_none.factor_at(s, t) - baseline.factor_at(s, t)).abs() < 1e-7,
                "empty-dlight factor must equal the static factor at ({s},{t})"
            );
        }
    }

    #[test]
    fn distant_light_below_minlight_does_not_contribute() {
        // A light far outside its radius range contributes nothing: rad < minlight
        // -> the face keeps its static borrow, no panic, no change.
        let (bsp, face, poly) = one_face_bsp_zplane(100);
        // dist 100000 >> radius 200, so rad < minlight -> no contribution.
        let dl = DynamicLight::new([0.0, 0.0, 100_000.0], 200.0, 10.0, 0.0, 0.0, 0);
        let lm = face_lightmap_dyn(&bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&dl)).expect("present");
        assert!(matches!(lm.luxels, Luxels::Static(_)), "a non-reaching light keeps the static borrow");
        let baseline = face_lightmap(&bsp, &face, &poly).expect("present");
        assert!((lm.factor_at(0.0, 0.0) - baseline.factor_at(0.0, 0.0)).abs() < 1e-7);
    }

    #[test]
    fn dynamic_light_factor_is_clamped() {
        // A huge-radius light right on the surface drives the luxel far past 255;
        // factor_at must clamp to MAX_LIGHT_FACTOR (finite, no overflow/NaN).
        let (bsp, face, poly) = one_face_bsp_zplane(255);
        let dl = DynamicLight::new([0.0, 0.0, 0.0], 100_000.0, 10.0, 0.0, 0.0, 0);
        let lm = face_lightmap_dyn(&bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&dl)).expect("present");
        let f = lm.factor_at(0.0, 0.0);
        assert!(f.is_finite());
        assert!((f - MAX_LIGHT_FACTOR).abs() < 1e-6, "huge add must clamp to {MAX_LIGHT_FACTOR}, got {f}");
    }

    // -- Animated light styles (R_BuildLightMap multi-style combine) -------

    /// A z-plane one-face BSP whose face uses two light styles. The LIGHTING
    /// lump concatenates the two `3x3` luxel blocks: block for `styles[0]` first
    /// (all `b0`), then `styles[1]` (all `b1`). `styles` are the style indices.
    fn two_style_face_bsp(
        styles: [u8; 4],
        b0: u8,
        b1: u8,
    ) -> (Bsp, crate::bsp::DFace, Vec<Vec3>) {
        // 9 luxels per block, two blocks concatenated.
        let mut lighting = vec![b0; 9];
        lighting.extend(std::iter::repeat(b1).take(9));
        let (mut bsp, mut face, poly) = one_face_bsp_zplane(b0);
        bsp.lighting = lighting;
        face.styles = styles;
        (bsp, face, poly)
    }

    #[test]
    fn single_steady_style0_neutral_is_static_and_byte_identical() {
        // A single steady style-0 face under neutral scales must keep the borrowed
        // static slice and sample exactly the static factor (no regression).
        let (bsp, face, poly) = one_face_bsp_zplane(200);
        let lm = face_lightmap_dyn(&bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, &[])
            .expect("present");
        assert!(
            matches!(lm.luxels, Luxels::Static(_)),
            "single steady style-0 at scale 1.0 keeps the static borrow"
        );
        let baseline = face_lightmap(&bsp, &face, &poly).expect("present");
        for &(s, t) in &[(0.0f32, 0.0f32), (8.0, 8.0), (16.0, 16.0), (32.0, 32.0)] {
            assert!(
                (lm.factor_at(s, t) - baseline.factor_at(s, t)).abs() < 1e-7,
                "neutral style-0 factor must equal the static factor at ({s},{t})"
            );
        }
    }

    #[test]
    fn second_flicker_style_changes_effective_luxel() {
        // styles[0]=0 (steady), styles[1]=1 (flicker). block0 = 100, block1 = 200.
        let (bsp, face, poly) = two_style_face_bsp([0, 1, 255, 255], 100, 200);

        // Style 1 dark (scale 0): effective = block0*1 + block1*0 = 100.
        let mut scales = NEUTRAL_LIGHTSTYLE_SCALES;
        scales[1] = 0.0;
        let dark = face_lightmap_dyn(&bsp, &face, &poly, &scales, &[]).expect("present");
        assert!(matches!(dark.luxels, Luxels::Owned(_)), "2-style face owns the combine");
        // Effective luxel 100 -> factor 100/255*2.
        assert!((dark.factor_at(0.0, 0.0) - (100.0 / 255.0 * 2.0)).abs() < 1e-5);

        // Style 1 normal (scale 1): effective = 100 + 200 = 300 -> clamps in factor.
        let mut scales_on = NEUTRAL_LIGHTSTYLE_SCALES;
        scales_on[1] = 1.0;
        let bright = face_lightmap_dyn(&bsp, &face, &poly, &scales_on, &[]).expect("present");
        let f_dark = dark.factor_at(0.0, 0.0);
        let f_bright = bright.factor_at(0.0, 0.0);
        assert!(
            f_bright > f_dark + 0.5,
            "raising style-1 scale must brighten the effective luxel: {f_dark} -> {f_bright}"
        );

        // And a partial scale lands strictly between (proves it scales the block).
        let mut scales_half = NEUTRAL_LIGHTSTYLE_SCALES;
        scales_half[1] = 0.5; // effective = 100 + 100 = 200
        let mid = face_lightmap_dyn(&bsp, &face, &poly, &scales_half, &[]).expect("present");
        assert!((mid.factor_at(0.0, 0.0) - (200.0 / 255.0 * 2.0)).abs() < 1e-5);
    }

    #[test]
    fn multi_style_too_short_lighting_falls_back_without_panic() {
        // The face declares two styles (needs 18 luxels) but only one block (9) is
        // present. The combine must NOT read out of bounds: it falls back to the
        // single static block (byte-identical to a steady style-0 face).
        let (mut bsp, mut face, poly) = one_face_bsp_zplane(123);
        face.styles = [0, 1, 255, 255]; // two active styles, but only 9 luxels stored
        bsp.lighting = vec![123u8; 9]; // one block only -> second block overruns
        let mut scales = NEUTRAL_LIGHTSTYLE_SCALES;
        scales[1] = 2.0; // would matter if the (missing) block were read
        // Must not panic; falls back to the single static block.
        let lm = face_lightmap_dyn(&bsp, &face, &poly, &scales, &[]).expect("present");
        assert!(
            matches!(lm.luxels, Luxels::Static(_)),
            "a too-short multi-style lump falls back to the static block borrow"
        );
        let baseline = face_lightmap(&bsp, &face, &poly).expect("present");
        assert!((lm.factor_at(0.0, 0.0) - baseline.factor_at(0.0, 0.0)).abs() < 1e-7);
    }

    #[test]
    fn unset_style_index_treated_as_normal() {
        // A face whose only style references an index whose scale is left at the
        // neutral 1.0 stays at full brightness (the "missing style -> normal"
        // rule). Style index 7, scale 1.0 (neutral): a single-style-at-1.0 face is
        // byte-identical to the static block.
        let (bsp, face, poly) = {
            let (mut bsp, mut face, poly) = one_face_bsp_zplane(180);
            face.styles = [7, 255, 255, 255];
            bsp.lighting = vec![180u8; 9];
            (bsp, face, poly)
        };
        let lm = face_lightmap_dyn(&bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, &[])
            .expect("present");
        // Single style at scale 1.0 -> static borrow, full brightness.
        assert!(matches!(lm.luxels, Luxels::Static(_)));
        assert!((lm.factor_at(0.0, 0.0) - (180.0 / 255.0 * 2.0)).abs() < 1e-6);
    }

    #[test]
    fn render_scene_ext_dlight_noop_on_fullbright_world() {
        // demo_room has no lighting lump, so its faces are fullbright (no
        // lightmap) and dlights cannot attach: the frame must be UNCHANGED even
        // with a bright light present. (The actual brightening of a lightmapped
        // surface is exercised by `dynamic_light_brightens_near_luxel_only`.)
        let bsp = demo_room();
        let pal = [[180u8, 180, 180]; 256];
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);

        let base = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES);
        // demo_room has no lighting lump, so faces are fullbright (no lightmap)
        // and dlights cannot attach; the frame must therefore be UNCHANGED even
        // with a light present -- proving dlights never touch non-lightmapped
        // faces and never panic.
        let dl = DynamicLight::new([0.0, 0.0, 0.0], 600.0, 10.0, 0.0, 0.0, 0);
        let lit = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], None, 0.0, &[], std::slice::from_ref(&dl), &NEUTRAL_LIGHTSTYLE_SCALES);
        assert_eq!(base.rgb, lit.rgb, "fullbright (lightmap-less) world must ignore dlights");
    }

    // -- PVS culling: decompress_vis / point_in_leaf -----------------------

    #[test]
    fn decompress_vis_rle_and_novis() {
        // Hand-built RLE stream for a map with numleafs = 20 (leaves 1..=20).
        //
        // Byte sequence:
        //   0xA5            -> leaves 1..8 from bits 1010_0101 (LSB=leaf1):
        //                      leaf1=1, leaf2=0, leaf3=1, leaf4=0,
        //                      leaf5=0, leaf6=1, leaf7=0, leaf8=1
        //   0x00 0x01       -> zero-run of 1 byte = 8 not-visible leaves (9..16)
        //   0xFF            -> leaves 17..20 all visible (only 4 consumed)
        let stream = [0xA5u8, 0x00, 0x01, 0xFF];
        let vis = decompress_vis(&stream, 0, 20);

        // Length is numleafs + 1, indexable by leaf number; leaf 0 always false.
        assert_eq!(vis.len(), 21);
        assert!(!vis[0], "leaf 0 (solid) is never visible");

        // First byte 0xA5 = 1010_0101.
        assert!(vis[1]);
        assert!(!vis[2]);
        assert!(vis[3]);
        assert!(!vis[4]);
        assert!(!vis[5]);
        assert!(vis[6]);
        assert!(!vis[7]);
        assert!(vis[8]);

        // Zero-run skipped leaves 9..=16.
        for l in 9..=16 {
            assert!(!vis[l], "leaf {l} should be in the zero-run (not visible)");
        }

        // Final 0xFF marks leaves 17..=20 visible.
        for l in 17..=20 {
            assert!(vis[l], "leaf {l} should be visible from the trailing 0xFF");
        }

        // visofs < 0 -> all leaves visible (Quake's mod_novis fallback).
        let all = decompress_vis(&stream, -1, 20);
        assert_eq!(all.len(), 21);
        assert!(all.iter().all(|&v| v), "no-vis fallback marks every leaf visible");
    }

    #[test]
    fn decompress_vis_truncated_is_safe() {
        // A zero byte with no following count byte must stop, not panic.
        let stream = [0x00u8];
        let vis = decompress_vis(&stream, 0, 16);
        assert_eq!(vis.len(), 17);
        // Nothing was marked visible; the decode simply stopped.
        assert!(vis.iter().all(|&v| !v));

        // An offset past the end of the lump also stops immediately (all false).
        let vis2 = decompress_vis(&stream, 99, 16);
        assert!(vis2.iter().all(|&v| !v));
    }

    /// Build a tiny BSP with exactly one splitting node and two leaves, so
    /// `point_in_leaf` has a well-defined front/back to resolve.
    ///
    /// Plane: normal +X, dist 0 (the YZ plane through the origin). `node.children`
    /// = `[-(leaf1)-1, -(leaf2)-1]` = `[-2, -3]`, so the front child (x >= 0) is
    /// leaf 1 and the back child (x < 0) is leaf 2. `models[0].headnode[0] = 0`.
    fn two_leaf_bsp() -> Bsp {
        use crate::bsp::{DLeaf, DNode, DPlane};
        let mut bsp = demo_room();
        bsp.planes = vec![DPlane {
            normal: [1.0, 0.0, 0.0],
            dist: 0.0,
            ptype: crate::bsp::PLANE_X,
        }];
        // children: front (x>=0) -> leaf index 1 => -(1)-1 = -2;
        //           back  (x<0)  -> leaf index 2 => -(2)-1 = -3.
        bsp.nodes = vec![DNode {
            planenum: 0,
            children: [-2, -3],
            mins: [0, 0, 0],
            maxs: [0, 0, 0],
            firstface: 0,
            numfaces: 0,
        }];
        // Three leaves: 0 = solid, 1 = front, 2 = back.
        let mk_leaf = |contents: i32| DLeaf {
            contents,
            visofs: -1,
            mins: [0, 0, 0],
            maxs: [0, 0, 0],
            firstmarksurface: 0,
            nummarksurfaces: 0,
            ambient_level: [0, 0, 0, 0],
        };
        bsp.leafs = vec![
            mk_leaf(crate::bsp::CONTENTS_SOLID),
            mk_leaf(crate::bsp::CONTENTS_EMPTY),
            mk_leaf(crate::bsp::CONTENTS_EMPTY),
        ];
        if let Some(m) = bsp.models.first_mut() {
            m.headnode = [0, 0, 0, 0];
        }
        bsp
    }

    #[test]
    fn point_in_leaf_resolves_plane_sides() {
        let bsp = two_leaf_bsp();

        // A point with x > 0 is on the front side (normal +X, dist 0) -> leaf 1.
        assert_eq!(point_in_leaf(&bsp, [10.0, 0.0, 0.0]), Some(1));
        // A point with x < 0 is on the back side -> leaf 2.
        assert_eq!(point_in_leaf(&bsp, [-10.0, 0.0, 0.0]), Some(2));
        // Exactly on the plane (d == 0) counts as front (>= 0) -> leaf 1.
        assert_eq!(point_in_leaf(&bsp, [0.0, 5.0, -3.0]), Some(1));
    }

    #[test]
    fn point_in_leaf_malformed_is_none() {
        // headnode pointing at a non-existent node yields None, not a panic.
        let mut bsp = two_leaf_bsp();
        if let Some(m) = bsp.models.first_mut() {
            m.headnode = [999, 0, 0, 0];
        }
        assert!(point_in_leaf(&bsp, [10.0, 0.0, 0.0]).is_none());

        // A node whose child points past the leaf array also yields None.
        let mut bsp2 = two_leaf_bsp();
        if let Some(n) = bsp2.nodes.first_mut() {
            n.children = [-9999, -3]; // front child -> leaf 9998, out of range
        }
        assert!(point_in_leaf(&bsp2, [10.0, 0.0, 0.0]).is_none());
    }

    #[test]
    fn compute_visible_faces_culls_unmarked_leaves() {
        // Build on the two-leaf BSP: give the worldmodel three faces, mark face 0
        // to leaf 1 and face 1 to leaf 2, leave face 2 unowned (submodel). Vis
        // for leaf 1 sees only itself, so face 1 (leaf 2 only) must be culled,
        // while face 0 (visible leaf) and face 2 (submodel) draw.
        use crate::bsp::DFace;
        let mut bsp = two_leaf_bsp();

        // Three trivial faces (their content is irrelevant to the masking test).
        let mk_face = || DFace {
            planenum: 0,
            side: 0,
            firstedge: 0,
            numedges: 4,
            texinfo: 0,
            styles: [0, 0, 0, 0],
            lightofs: -1,
        };
        bsp.faces = vec![mk_face(), mk_face(), mk_face()];
        // marksurfaces: [face0, face1]; leaf1 -> {0}, leaf2 -> {1}; face2 unowned.
        bsp.marksurfaces = vec![0, 1];
        bsp.leafs[1].firstmarksurface = 0;
        bsp.leafs[1].nummarksurfaces = 1; // leaf1 owns face 0
        bsp.leafs[1].visofs = 0; // leaf1 has vis info at byte 0
        bsp.leafs[2].firstmarksurface = 1;
        bsp.leafs[2].nummarksurfaces = 1; // leaf2 owns face 1

        // PVS for leaf 1 (numleafs = 2): a single byte with only leaf 1's bit set
        // (bit 0 = leaf 1, bit 1 = leaf 2) -> 0b01 = 0x01: leaf1 visible, leaf2 not.
        bsp.visibility = vec![0x01];

        // Camera in the front half-space resolves to leaf 1.
        let mask = compute_visible_faces(&bsp, [10.0, 0.0, 0.0])
            .expect("a real leaf with vis should produce a culling mask");
        assert_eq!(mask.len(), 3);
        assert!(mask[0], "face 0 (in the visible view leaf) should draw");
        assert!(!mask[1], "face 1 (only in the culled leaf 2) should be culled");
        assert!(mask[2], "face 2 (unowned/submodel) should always draw");

        // No visibility lump -> no culling (None means draw everything).
        let mut novis = bsp.clone();
        novis.visibility = Vec::new();
        assert!(compute_visible_faces(&novis, [10.0, 0.0, 0.0]).is_none());
    }

    #[test]
    fn demo_room_pvs_is_noop() {
        // demo_room has no visibility lump, so PVS must not cull anything: the
        // textured render is identical with the culling code present.
        let bsp = demo_room();
        assert!(bsp.visibility.is_empty(), "demo_room has no vis lump");
        // compute_visible_faces returns None (no culling) for such a map.
        assert!(compute_visible_faces(&bsp, [0.0, 0.0, 0.0]).is_none());
    }

    // -- Brush submodels (inline `*N` bmodels: doors / plats / buttons) -----

    /// Extend [`demo_room`] with a second inline model (model index 1): a single
    /// 64x64 quad whose face plane faces `-X`. The quad's vertices live in the
    /// shared vertex array in MODEL-LOCAL space (a YZ square at local `x = 0`),
    /// exactly as Quake stores bmodel geometry, so a caller places it by adding
    /// an `origin`.
    ///
    /// `model 0` is left covering only the original world faces (its `numfaces`
    /// is unchanged); the new quad is owned solely by `model 1`.
    fn demo_room_with_submodel() -> Bsp {
        use crate::bsp::{DEdge, DFace, DModel, DPlane};

        let mut bsp = demo_room();

        // Record where the world's faces end; the submodel face starts here.
        let submodel_firstface = bsp.faces.len() as i32;

        // Four corners of a 64x64 YZ quad at local x = 0, ordered CCW as seen
        // from -X. Pushed as fresh vertexes in the SHARED vertex array.
        let base_vtx = bsp.vertexes.len() as u16;
        let corners: [[f32; 3]; 4] = [
            [0.0, -32.0, -32.0],
            [0.0, 32.0, -32.0],
            [0.0, 32.0, 32.0],
            [0.0, -32.0, 32.0],
        ];
        for c in corners {
            bsp.vertexes.push(crate::bsp::DVertex { point: c });
        }

        // Four edges around the quad, referenced by four positive surfedges.
        let first_edge = bsp.surfedges.len() as i32;
        for k in 0..4u16 {
            let a = base_vtx + k;
            let b = base_vtx + ((k + 1) % 4);
            let edge_index = bsp.edges.len() as i32;
            bsp.edges.push(DEdge { v: [a, b] });
            bsp.surfedges.push(edge_index);
        }

        // Plane: outward normal -X (faces toward a camera on the -X side).
        let planenum = bsp.planes.len() as i16;
        bsp.planes.push(DPlane {
            normal: [-1.0, 0.0, 0.0],
            dist: 0.0,
            ptype: crate::bsp::PLANE_X,
        });

        // Reuse texinfo 0 (axis-aligned; demo_room has no inline textures so the
        // submodel takes the flat-colour fallback, same as the world walls).
        bsp.faces.push(DFace {
            planenum,
            side: 0,
            firstedge: first_edge,
            numedges: 4,
            texinfo: 0,
            styles: [0, 0, 0, 0],
            lightofs: -1,
        });

        // Model 1: just the new quad face.
        bsp.models.push(DModel {
            mins: [-1.0, -32.0, -32.0],
            maxs: [1.0, 32.0, 32.0],
            origin: [0.0, 0.0, 0.0],
            headnode: [0, 0, 0, 0],
            visleafs: 0,
            firstface: submodel_firstface,
            numfaces: 1,
        });

        bsp
    }

    #[test]
    fn render_scene_ext_draws_submodel() {
        // A submodel placed in front of the camera must add non-background
        // pixels relative to an empty bmodel list (the submodel becomes visible).
        let bsp = demo_room_with_submodel();
        let pal = [[200u8, 200, 200]; 256];
        // Look down +X from near the west wall.
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let bg = [10u8, 10, 14];

        let without = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES);
        let with = render_scene_ext(
            &bsp,
            &cam,
            160,
            120,
            &pal,
            &[],
            // Place the quad between the camera (-200) and the centre, facing it.
            &[BModelInstance { model_index: 1, origin: [-120.0, 0.0, 0.0] }],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
        );

        let drawn_without = without.rgb.iter().filter(|&&p| p != bg).count();
        let drawn_with = with.rgb.iter().filter(|&&p| p != bg).count();
        assert!(
            drawn_with > drawn_without,
            "submodel should add visible pixels: {drawn_without} -> {drawn_with}"
        );

        // And it must actually change the framebuffer somewhere.
        let changed = without
            .rgb
            .iter()
            .zip(with.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "submodel changed no pixels");
    }

    #[test]
    fn render_scene_ext_out_of_range_submodel_is_noop() {
        // An out-of-range model_index must draw nothing and not panic: the image
        // is byte-identical to passing an empty bmodel list.
        let bsp = demo_room_with_submodel();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);

        let empty = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES);
        let oob = render_scene_ext(
            &bsp,
            &cam,
            160,
            120,
            &pal,
            &[],
            &[BModelInstance { model_index: 999, origin: [-120.0, 0.0, 0.0] }],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
        );
        assert_eq!(
            empty.rgb, oob.rgb,
            "out-of-range submodel index must be a no-op"
        );
    }

    #[test]
    fn render_scene_matches_ext_empty_on_demo_room() {
        // render_scene must equal render_scene_ext(.., &[]) on demo_room: the
        // wrapper preserves the existing world+alias behaviour exactly.
        let bsp = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);

        // No alias models, no bmodels.
        let a = render_scene(&bsp, &cam, 160, 120, &pal, &[]);
        let b = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES);
        assert_eq!(a.rgb, b.rgb, "render_scene must equal render_scene_ext(.., &[])");

        // Also holds with an alias instance present (the model path is shared).
        let mdl = tiny_mdl();
        let inst = ModelInstance {
            mdl: &mdl,
            origin: [-80.0, 0.0, 0.0],
            yaw: 0.0,
            frame: 0,
            color: [255, 32, 32],
        };
        let a2 = render_scene(&bsp, &cam, 160, 120, &pal, std::slice::from_ref(&inst));
        let b2 = render_scene_ext(
            &bsp,
            &cam,
            160,
            120,
            &pal,
            std::slice::from_ref(&inst),
            &[],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
        );
        assert_eq!(
            a2.rgb, b2.rgb,
            "render_scene must equal render_scene_ext with the same alias models and no bmodels"
        );
    }

    #[test]
    fn submodel_origin_shifts_geometry() {
        // The same submodel at two different origins must land in different
        // places: rendering it at one origin vs another changes pixels. This
        // proves the origin shift actually moves the geometry.
        let bsp = demo_room_with_submodel();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);

        let centered = render_scene_ext(
            &bsp,
            &cam,
            160,
            120,
            &pal,
            &[],
            &[BModelInstance { model_index: 1, origin: [-120.0, 0.0, 0.0] }],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
        );
        // Shift the quad well off to one side (+Y) so it projects elsewhere.
        let shifted = render_scene_ext(
            &bsp,
            &cam,
            160,
            120,
            &pal,
            &[],
            &[BModelInstance { model_index: 1, origin: [-120.0, 120.0, 0.0] }],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
        );
        let changed = centered
            .rgb
            .iter()
            .zip(shifted.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "moving the submodel origin should move its pixels");
    }

    #[test]
    fn submodel_tolerates_malformed_faces() {
        // A submodel whose faces reference out-of-range edges/planes/texinfo must
        // be skipped without panicking (mirrors render_tolerates_malformed_faces
        // for the world path).
        let mut bsp = demo_room_with_submodel();
        // Corrupt the submodel's single face (the last face in the array).
        if let Some(f) = bsp.faces.last_mut() {
            f.firstedge = 1_000_000; // past surfedges
            f.planenum = 30_000; // past planes
            f.texinfo = 30_000; // past texinfo
        }
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        // Must not panic; the corrupt face is simply skipped.
        let _img = render_scene_ext(
            &bsp,
            &cam,
            80,
            60,
            &pal,
            &[],
            &[BModelInstance { model_index: 1, origin: [-120.0, 0.0, 0.0] }],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
        );
    }

    // -- First-person weapon viewmodel (camera-anchored, drawn on top) --------

    /// A small single-skin, single-frame MDL whose one triangle sits *forward*
    /// of the model origin (model `+X` is the gun's forward axis), so once the
    /// viewmodel anchors it to the camera basis every vertex lands in front of
    /// the near plane and the triangle actually rasterises. Coloured via a 1x1
    /// skin so it takes the textured path through `palette[7]`.
    fn viewmodel_mdl() -> crate::mdl::Mdl {
        use crate::mdl::{AliasFrame, Frame, Mdl, MdlHeader, Skin, StVert, Triangle, TriVertex};
        // Mirror the real `v_*` weapon layout: forward along model `+X`, thin in
        // `+Y`, and sitting *below* the eye (model `Z < 0`, via `scale_origin`).
        // So after the camera-anchor + lower-centre offset the gun lands in the
        // lower half of the frame, like the shipping weapon models.
        let header = MdlHeader {
            ident: i32::from_le_bytes(*b"IDPO"),
            version: 6,
            scale: [1.0, 1.0, 1.0],
            scale_origin: [10.0, 0.0, -10.0],
            boundingradius: 64.0,
            eyeposition: [0.0, 0.0, 0.0],
            numskins: 1,
            skinwidth: 1,
            skinheight: 1,
            numverts: 3,
            numtris: 1,
            numframes: 1,
            synctype: 0,
            flags: 0,
            size: 1.0,
        };
        // Decoded model space: X in [10, 26] (forward), Z in [-10, -2] (below the
        // eye). With OFS_FORWARD = 30 every vertex sits well in front of the near
        // plane at any yaw, so the triangle always rasterises.
        let verts = vec![
            TriVertex { v: [0, 0, 0], lightnormalindex: 0 },
            TriVertex { v: [16, 0, 0], lightnormalindex: 0 },
            TriVertex { v: [8, 0, 8], lightnormalindex: 0 },
        ];
        Mdl {
            header,
            skins: vec![Skin::Single(vec![7])],
            stverts: vec![StVert { onseam: 0, s: 0, t: 0 }; 3],
            triangles: vec![Triangle { facesfront: 1, vertindex: [0, 1, 2] }],
            frames: vec![Frame::Single(AliasFrame {
                name: "v0".into(),
                bboxmin: TriVertex { v: [0, 0, 0], lightnormalindex: 0 },
                bboxmax: TriVertex { v: [16, 0, 8], lightnormalindex: 0 },
                verts,
            })],
        }
    }

    /// True when a pixel looks like the viewmodel's skin: palette index 7 is set
    /// to pure yellow `[255, 255, 0]` in these tests, and the only per-pixel
    /// transform is a multiply by the (positive) Lambert `shade`. So a gun pixel
    /// keeps `B == 0` with `R > 0` and `G > 0`, whereas `hash_color` walls (HSV
    /// saturation 0.55) always have all three channels strictly positive and the
    /// background `[10,10,14]` has `B != 0`.
    fn is_gun_pixel(p: [u8; 3]) -> bool {
        p[2] == 0 && p[0] > 0 && p[1] > 0
    }

    /// The bounding box (min_x, min_y, max_x, max_y) of the pixels that differ
    /// from `bg`, plus their centroid. Returns `None` when nothing was drawn.
    fn drawn_bbox(img: &Image, bg: [u8; 3]) -> Option<(usize, usize, usize, usize, f32, f32)> {
        let (mut minx, mut miny, mut maxx, mut maxy) = (usize::MAX, usize::MAX, 0usize, 0usize);
        let (mut sx, mut sy, mut n) = (0f64, 0f64, 0u64);
        for y in 0..img.h {
            for x in 0..img.w {
                if img.rgb[y * img.w + x] != bg {
                    minx = minx.min(x);
                    miny = miny.min(y);
                    maxx = maxx.max(x);
                    maxy = maxy.max(y);
                    sx += x as f64;
                    sy += y as f64;
                    n += 1;
                }
            }
        }
        if n == 0 {
            None
        } else {
            Some((minx, miny, maxx, maxy, (sx / n as f64) as f32, (sy / n as f64) as f32))
        }
    }

    #[test]
    fn viewmodel_is_camera_anchored_not_world_anchored() {
        // Drawing the viewmodel at two very different camera yaws must place the
        // gun in roughly the SAME lower-centre screen region both times — proving
        // it is anchored to the view, not to a world position (which would swing
        // wildly across the frame, or vanish, as the camera turns).
        let bsp = demo_room();
        let mut pal = [[0u8; 3]; 256];
        pal[7] = [255, 255, 0]; // the viewmodel's skin colour (index 7), pure yellow
        let bg = [10u8, 10, 14];
        let (w, h) = (160usize, 120usize);
        let gun = viewmodel_mdl();

        // Two cameras at the room centre, looking in very different directions.
        let cam_a = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, fov_deg: 90.0 };
        let cam_b = Camera { pos: [0.0, 0.0, 0.0], yaw: 137.0, pitch: 0.0, fov_deg: 90.0 };

        let img_a = render_scene_ext(
            &bsp, &cam_a, w, h, &pal, &[], &[],
            Some(Viewmodel { mdl: &gun, frame: 0 }),
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
        );
        let img_b = render_scene_ext(
            &bsp, &cam_b, w, h, &pal, &[], &[],
            Some(Viewmodel { mdl: &gun, frame: 0 }),
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
        );

        // Isolate the gun pixels (its unique skin colour) in each frame.
        let gun_only = |img: &Image| {
            let mut g = Image::new(img.w, img.h, bg);
            for i in 0..img.rgb.len() {
                if is_gun_pixel(img.rgb[i]) {
                    g.rgb[i] = [255, 255, 0];
                }
            }
            g
        };
        let ga = gun_only(&img_a);
        let gb = gun_only(&img_b);
        let (_, _, _, _, cax, cay) = drawn_bbox(&ga, bg).expect("gun visible at yaw A");
        let (_, _, _, _, cbx, cby) = drawn_bbox(&gb, bg).expect("gun visible at yaw B");

        // The gun centroid must land in the lower-centre band in BOTH frames and
        // move only a little between the two wildly different yaws.
        let cxf = w as f32 / 2.0;
        for (cx, cy) in [(cax, cay), (cbx, cby)] {
            assert!(
                (cx - cxf).abs() < w as f32 * 0.30,
                "gun should be roughly horizontally centred (cx={cx}, centre={cxf})"
            );
            assert!(
                cy > h as f32 * 0.5,
                "gun should sit in the lower half of the frame (cy={cy}, h={h})"
            );
        }
        assert!(
            (cax - cbx).abs() < w as f32 * 0.15 && (cay - cby).abs() < h as f32 * 0.15,
            "view-anchored gun should barely move between yaws: A=({cax},{cay}) B=({cbx},{cby})"
        );
    }

    #[test]
    fn viewmodel_draws_on_top_of_a_wall() {
        // With a wall directly in front of the camera, the viewmodel must still
        // be visible: its skin colour appears in the frame even though world
        // geometry fills the same pixels. (A depth-tested-against-world gun would
        // be hidden by the near wall.)
        let bsp = demo_room();
        let mut pal = [[80u8; 3]; 256]; // (walls use hash_color, not the palette)
        pal[7] = [255, 255, 0]; // distinctive pure-yellow gun colour
        let (w, h) = (160usize, 120usize);
        let gun = viewmodel_mdl();

        // Stand close to the east wall (x = 256) looking straight at it (+X), so
        // a wall is right in front and would occlude a depth-tested viewmodel.
        // The gun is anchored ~40-60 units ahead, i.e. world x ~240-260, AT the
        // wall plane — depth-tested it would lose, but it must still show.
        let cam = Camera { pos: [200.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, fov_deg: 90.0 };

        // Sanity: the wall actually fills the view (without the gun).
        let world = render_scene_ext(&bsp, &cam, w, h, &pal, &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES);
        let bg = [10u8, 10, 14];
        let wall_pixels = world.rgb.iter().filter(|&&p| p != bg).count();
        assert!(wall_pixels > w * h / 2, "expected the wall to fill most of the view");
        // The wall must NOT itself produce gun-coloured pixels (so the assert
        // below truly measures the gun, not the wall).
        assert!(
            !world.rgb.iter().any(|&p| is_gun_pixel(p)),
            "wall-only render must not contain gun-coloured pixels"
        );

        let with_gun = render_scene_ext(
            &bsp, &cam, w, h, &pal, &[], &[],
            Some(Viewmodel { mdl: &gun, frame: 0 }),
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
        );

        // The gun's pure-yellow skin (B == 0) must appear, proving it drew on top
        // of the wall rather than being depth-occluded by it.
        let shows_gun = with_gun.rgb.iter().any(|&p| is_gun_pixel(p));
        assert!(shows_gun, "weapon viewmodel must draw on top of the wall directly ahead");

        // And it changed pixels relative to the wall-only render.
        let changed = world
            .rgb
            .iter()
            .zip(with_gun.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "viewmodel changed no pixels over the wall");
    }

    #[test]
    fn viewmodel_none_matches_no_viewmodel() {
        // Passing `None` for the viewmodel must be byte-identical to the prior
        // behaviour (render_scene_ext with the trailing arg absent in spirit).
        let bsp = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let a = render_scene(&bsp, &cam, 160, 120, &pal, &[]);
        let b = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES);
        assert_eq!(a.rgb, b.rgb, "None viewmodel must equal render_scene");
    }

    #[test]
    fn viewmodel_tolerates_malformed_model() {
        // A weapon model with out-of-range triangle indices and no frames must be
        // skipped without panicking and without altering the frame.
        let bsp = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, fov_deg: 90.0 };

        // Frameless model -> draw_viewmodel returns early.
        let mut frameless = viewmodel_mdl();
        frameless.frames.clear();
        let img = render_scene_ext(
            &bsp, &cam, 80, 60, &pal, &[], &[],
            Some(Viewmodel { mdl: &frameless, frame: 0 }),
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
        );
        let baseline = render_scene_ext(&bsp, &cam, 80, 60, &pal, &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES);
        assert_eq!(img.rgb, baseline.rgb, "frameless weapon must draw nothing");

        // Out-of-range triangle vertex index -> that triangle is skipped.
        let mut bad = viewmodel_mdl();
        bad.triangles = vec![crate::mdl::Triangle { facesfront: 1, vertindex: [0, 1, 9999] }];
        // Must not panic.
        let _ = render_scene_ext(
            &bsp, &cam, 80, 60, &pal, &[], &[],
            Some(Viewmodel { mdl: &bad, frame: 0 }),
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
        );
    }

    // -- Animated special surfaces: turbulent liquid warp + scrolling sky -----

    #[test]
    fn surface_classification_by_name() {
        // Liquids begin with '*'; sky begins with 'sky' (case-insensitive);
        // everything else is an ordinary wall.
        assert_eq!(classify_surface("*water1"), SurfKind::Turb);
        assert_eq!(classify_surface("*lava1"), SurfKind::Turb);
        assert_eq!(classify_surface("*slime"), SurfKind::Turb);
        assert_eq!(classify_surface("*teleport"), SurfKind::Turb);
        assert_eq!(classify_surface("sky1"), SurfKind::Sky);
        assert_eq!(classify_surface("sky4"), SurfKind::Sky);
        assert_eq!(classify_surface("SKY1"), SurfKind::Sky);
        assert_eq!(classify_surface("wall_brick"), SurfKind::Normal);
        assert_eq!(classify_surface("city4_7"), SurfKind::Normal);
        // Short / edge-case names never panic and default sensibly.
        assert_eq!(classify_surface(""), SurfKind::Normal);
        assert_eq!(classify_surface("sk"), SurfKind::Normal);
        assert_eq!(classify_surface("*"), SurfKind::Turb);
    }

    #[test]
    fn turb_table_amplitude_and_wrap() {
        // The table swings ±AMP and indexing is masked, so any coord is in range.
        let turb = TurbTable::new();
        // sin(0) = 0 at index 0.
        assert!(turb.sin[0].abs() < 1e-5);
        // Peak magnitude is exactly the amplitude.
        let max = turb.sin.iter().cloned().fold(f32::MIN, f32::max);
        let min = turb.sin.iter().cloned().fold(f32::MAX, f32::min);
        assert!((max - TURB_AMP).abs() < 1e-3, "peak should be +AMP, got {max}");
        assert!((min + TURB_AMP).abs() < 1e-3, "trough should be -AMP, got {min}");
        // `at` never panics for huge / negative / non-finite inputs, and the
        // result stays within the table's range [-AMP, AMP].
        for &c in &[0.0, 1e9, -1e9, f32::INFINITY, f32::NAN, 12345.6] {
            let v = turb.at(c);
            assert!(v.is_finite());
            assert!(v.abs() <= TURB_AMP + 1e-3);
        }
    }

    #[test]
    fn warp_st_animates_and_stays_bounded() {
        // The turbulent warp must MOVE the sampled (s,t) as time advances (so the
        // surface visibly ripples), and the displacement is bounded by ±AMP on
        // each axis (so a tiling texture's rem_euclid keeps it in range).
        let turb = TurbTable::new();
        let (s, t) = (20.0f32, 33.0f32);
        let (s0, t0) = warp_st(&turb, s, t, 0.0);
        let (s1, t1) = warp_st(&turb, s, t, 0.37);
        // Animated: at least one axis differs between the two times.
        assert!(
            (s0 - s1).abs() > 1e-4 || (t0 - t1).abs() > 1e-4,
            "warp should change the sample between two times: ({s0},{t0}) vs ({s1},{t1})"
        );
        // Bounded: the displacement off the base coordinate is at most ±AMP.
        for (warped, base) in [(s1, s), (t1, t)] {
            assert!((warped - base).abs() <= TURB_AMP + 1e-3);
        }
    }

    /// Build a synthetic 64x64 "liquid" miptexture: a vivid gradient of palette
    /// indices so a small change in the sampled (s,t) lands on a different index.
    fn synthetic_liquid_pixels() -> Vec<u8> {
        let mut px = vec![0u8; 64 * 64];
        for y in 0..64usize {
            for x in 0..64usize {
                // A non-trivial pattern: index depends on both axes.
                px[y * 64 + x] = ((x * 4 + y * 7) % 256) as u8;
            }
        }
        px
    }

    #[test]
    fn turbulent_sampler_animates_at_fixed_st() {
        // Drive `raster_triangle_tex` in Turb mode over a single screen-filling
        // triangle and confirm that sampling the SAME geometry at two different
        // `time` values produces a DIFFERENT framebuffer (it animates), while
        // every sampled index stays in bounds (no panic, no garbage).
        let turb = TurbTable::new();
        let pixels = synthetic_liquid_pixels();
        // A palette that maps each index to a distinct grey so different texels
        // give different colours.
        let mut pal = [[0u8; 3]; 256];
        for (i, p) in pal.iter_mut().enumerate() {
            *p = [i as u8, i as u8, i as u8];
        }

        // One large triangle covering the framebuffer, spanning a range of (s,t)
        // so the warp samples many texels.
        let (w, h) = (40usize, 40usize);
        let render_at = |time: f32| {
            let mut img = Image::new(w, h, [0, 0, 0]);
            let mut zb = vec![f32::INFINITY; w * h];
            let v0 = ProjT { x: 0.0, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
            let v1 = ProjT { x: w as f32, y: 0.0, vz: 1.0, s: 128.0, t: 0.0 };
            let v2 = ProjT { x: 0.0, y: h as f32, vz: 1.0, s: 0.0, t: 128.0 };
            raster_triangle_tex(
                &mut img, &mut zb, v0, v1, v2,
                &pixels, 64, 64, &pal, 1.0, None,
                SurfaceMode::Turb { turb: &turb, time },
            );
            img
        };
        let a = render_at(0.0);
        let b = render_at(0.5);

        // Animated: the two frames must differ somewhere.
        let changed = a.rgb.iter().zip(b.rgb.iter()).filter(|(x, y)| x != y).count();
        assert!(changed > 0, "turbulent surface must animate between two times");

        // Every drawn pixel is a real palette colour (grey: all channels equal),
        // proving the sample stayed in bounds (out-of-range would have continued).
        assert!(
            a.rgb.iter().any(|p| *p != [0, 0, 0]),
            "turbulent triangle drew nothing"
        );
        for p in a.rgb.iter().chain(b.rgb.iter()) {
            assert!(p[0] == p[1] && p[1] == p[2], "sampled colour not a palette grey: {p:?}");
        }
    }

    /// Build a synthetic 256x128 sky miptexture: the LEFT half (the alpha overlay)
    /// is index 0 (transparent) in a band and a vivid index elsewhere; the RIGHT
    /// half (the solid background) is a gradient. So compositing shows the
    /// background through the transparent overlay band.
    fn synthetic_sky_pixels() -> Vec<u8> {
        let mut px = vec![0u8; 256 * 128];
        for y in 0..128usize {
            for x in 0..128usize {
                // Left (overlay) half: transparent (0) in the left third, else 200.
                px[y * 256 + x] = if x < 42 { 0 } else { 200 };
                // Right (background) half: a non-zero gradient, never 0.
                px[y * 256 + (128 + x)] = (1 + ((x + y) % 200)) as u8;
            }
        }
        px
    }

    #[test]
    fn sky_sampler_renders_nonbackground_and_animates() {
        // The sky sampler must (1) produce real (non-framebuffer-background)
        // pixels — i.e. show the sky texture, not a flat fill — and (2) differ
        // between two times (it scrolls).
        let pixels = synthetic_sky_pixels();
        let mut pal = [[0u8; 3]; 256];
        for (i, p) in pal.iter_mut().enumerate() {
            // Map every index to a distinct, clearly non-zero colour so any
            // sampled sky texel is visibly different from the [0,0,0] background.
            *p = [(i as u8).max(1), 255u8.saturating_sub(i as u8), 128];
        }

        let (w, h) = (48usize, 48usize);
        let render_at = |time: f32| {
            let mut img = Image::new(w, h, [0, 0, 0]); // background = pure black
            let mut zb = vec![f32::INFINITY; w * h];
            // A triangle spanning a wide (s,t) so the scroll samples many texels.
            let v0 = ProjT { x: 0.0, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
            let v1 = ProjT { x: w as f32, y: 0.0, vz: 1.0, s: 512.0, t: 0.0 };
            let v2 = ProjT { x: 0.0, y: h as f32, vz: 1.0, s: 0.0, t: 512.0 };
            raster_triangle_tex(
                &mut img, &mut zb, v0, v1, v2,
                &pixels, 256, 128, &pal, 1.0, None,
                SurfaceMode::Sky { time },
            );
            img
        };
        let a = render_at(0.0);
        let b = render_at(1.0);

        // (1) Non-background: the sky drew real texels (not a flat empty frame).
        let drawn = a.rgb.iter().filter(|&&p| p != [0, 0, 0]).count();
        assert!(drawn > 0, "sky face rendered no pixels (should show the sky texture)");

        // (2) Animated: scrolling shifts the texels, so the two frames differ.
        let changed = a.rgb.iter().zip(b.rgb.iter()).filter(|(x, y)| x != y).count();
        assert!(changed > 0, "sky must scroll (differ) between two times");
    }

    #[test]
    fn sky_texel_composites_overlay_over_background() {
        // Where the overlay (left half) is transparent (index 0), the background
        // (right half) shows through; where the overlay is opaque, it wins. At
        // time 0 there is no scroll, so the layout maps directly.
        let pixels = synthetic_sky_pixels();
        let tw = 256usize;
        let th = 128usize;

        // sky_texel scales (s,t) by 0.125 internally, so to land on overlay
        // column `c` (in [0,128)) we pass s = c/0.125 = c*8.
        // Column 0 of the overlay is transparent (x<42) -> shows the background's
        // column 0 (= 1 + (0+0)%200 = 1).
        let at0 = sky_texel(&pixels, tw, th, 0.0, 0.0, 0.0);
        assert_eq!(at0, 1, "transparent overlay should reveal background texel");

        // Column 64 of the overlay is opaque (x>=42) -> the overlay value 200.
        let at64 = sky_texel(&pixels, tw, th, 64.0 * 8.0, 0.0, 0.0);
        assert_eq!(at64, 200, "opaque overlay texel should win over background");

        // A degenerate (too-small) sky texture never panics and returns index 0
        // (everything out of range).
        let tiny = vec![0u8; 4];
        assert_eq!(sky_texel(&tiny, 2, 2, 1e6, -1e6, 5.0), 0);
    }

    #[test]
    fn special_surfaces_animate_in_full_render() {
        // End-to-end: a room whose floor is a liquid and whose ceiling is sky
        // must render those faces (non-background) and the frame must DIFFER
        // between two game times — water ripples and sky scrolls together.
        let bsp = special_surface_room();
        // A palette mapping each index to a distinct colour.
        let mut pal = [[0u8; 3]; 256];
        for (i, p) in pal.iter_mut().enumerate() {
            *p = [(i as u8).max(1), 255u8.saturating_sub(i as u8), (i as u8) ^ 0x55];
        }
        // Stand high near the centre looking down at the floor (the liquid),
        // which fills the frame, so the turbulent warp has plenty of texels to
        // ripple. (The ceiling sky is also a special face; either animating is
        // enough for this assertion.)
        let cam = Camera::looking_at([0.0, 0.0, 100.0], [0.0, 0.0, -128.0], 90.0);

        let a = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES);
        let b = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], None, 0.6, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES);

        let bg = [10u8, 10, 14];
        assert!(
            a.rgb.iter().any(|&p| p != bg),
            "special-surface room rendered nothing"
        );
        let changed = a.rgb.iter().zip(b.rgb.iter()).filter(|(x, y)| x != y).count();
        assert!(
            changed > 0,
            "liquid/sky faces must animate between two game times"
        );
    }

    #[test]
    fn demo_room_unaffected_by_time() {
        // demo_room has no special textures (no inline miptex at all), so it must
        // take the Normal path and render IDENTICALLY at any time — the animation
        // never touches ordinary walls.
        let bsp = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let t0 = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES);
        let t1 = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], None, 9.5, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES);
        assert_eq!(t0.rgb, t1.rgb, "ordinary walls must not animate with time");
        // And it must equal the time-less render_scene wrapper.
        let rs = render_scene(&bsp, &cam, 160, 120, &pal, &[]);
        assert_eq!(t0.rgb, rs.rgb, "render_scene must equal render_scene_ext(.., 0.0)");
    }

    /// A [`demo_room`] whose FLOOR is a liquid (`*water1`) and CEILING is sky
    /// (`sky1`), each backed by a synthetic inline miptexture, so the world pass
    /// routes those two faces through the turbulent / sky animated samplers while
    /// the four walls stay ordinary. Used to prove special surfaces animate.
    fn special_surface_room() -> Bsp {
        use crate::bsp::{MipTex, TexInfo, TEX_SPECIAL};
        let mut bsp = demo_room();

        // Two inline miptextures: index 0 = liquid (64x64), index 1 = sky (256x128).
        let liquid = MipTex {
            name: "*water1".into(),
            width: 64,
            height: 64,
            offsets: [0, 0, 0, 0],
            pixels: synthetic_liquid_pixels(),
        };
        let sky = MipTex {
            name: "sky1".into(),
            width: 256,
            height: 128,
            offsets: [0, 0, 0, 0],
            pixels: synthetic_sky_pixels(),
        };
        bsp.textures = vec![Some(liquid), Some(sky)];

        // Rebuild texinfo: an axis-aligned set where miptex 0 (liquid) and miptex
        // 1 (sky) are both flagged TEX_SPECIAL; the rest reuse miptex 2 (absent ->
        // flat fallback, unchanged Normal walls). The floor uses texinfo 0, the
        // ceiling uses texinfo 1 (matching demo_room's add_quad ordering: floor is
        // the first face with texinfo 0, ceiling the second with texinfo 1).
        let axis = |miptex: i32, flags: i32| TexInfo {
            vecs: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            miptex,
            flags,
        };
        // texinfo 0 -> liquid (special); texinfo 1 -> sky (special); 2.. -> normal.
        let mut tex = vec![axis(0, TEX_SPECIAL), axis(1, TEX_SPECIAL)];
        for _ in 2..9 {
            tex.push(axis(2, 0));
        }
        bsp.texinfo = tex;
        bsp
    }

    // -- HUD / status bar -----------------------------------------------------

    use crate::wad::{Qpic, Wad2, CMP_NONE, LUMPINFO_SIZE, NAME_LEN, TYP_QPIC, WADINFO_SIZE};

    /// A test palette where index `i` maps to the RGB `[i, i, i]` (so a texel's
    /// palette index is recoverable from any channel of the drawn pixel). Index
    /// 255 stays the transparent colour and is never blitted.
    fn ramp_palette() -> [[u8; 3]; 256] {
        let mut p = [[0u8; 3]; 256];
        for (i, px) in p.iter_mut().enumerate() {
            *px = [i as u8, i as u8, i as u8];
        }
        p
    }

    /// One synthetic qpic payload: width i32, height i32, then `w*h` indices.
    fn qpic_payload(w: i32, h: i32, fill: u8) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&w.to_le_bytes());
        v.extend_from_slice(&h.to_le_bytes());
        v.resize(8 + (w as usize) * (h as usize), fill);
        v
    }

    /// Append a 32-byte `lumpinfo_t` entry (mirrors `wad.rs`'s test helper).
    fn push_lump(dir: &mut Vec<u8>, filepos: i32, size: i32, name: &str) {
        dir.extend_from_slice(&filepos.to_le_bytes());
        dir.extend_from_slice(&size.to_le_bytes()); // disksize
        dir.extend_from_slice(&size.to_le_bytes()); // size
        dir.push(TYP_QPIC);
        dir.push(CMP_NONE);
        dir.push(0); // pad1
        dir.push(0); // pad2
        let mut field = [0u8; NAME_LEN];
        let nb = name.as_bytes();
        let n = nb.len().min(NAME_LEN);
        field[..n].copy_from_slice(&nb[..n]);
        dir.extend_from_slice(&field);
    }

    /// Build a synthetic `gfx.wad` containing `sbar` (320x24), `num_0..num_9`
    /// (24x24, each filled with palette index `100+d` so digits are recognisable
    /// and never transparent), and `anum_0..anum_9` (24x24, index `120+d`).
    fn build_hud_wad() -> Wad2 {
        // (name, payload) pairs.
        let mut pics: Vec<(String, Vec<u8>)> = Vec::new();
        pics.push(("sbar".to_string(), qpic_payload(320, 24, 1)));
        for d in 0..10u8 {
            pics.push((format!("num_{d}"), qpic_payload(24, 24, 100 + d)));
        }
        for d in 0..10u8 {
            pics.push((format!("anum_{d}"), qpic_payload(24, 24, 120 + d)));
        }

        // Lay payloads right after the 12-byte header; build the directory after.
        let mut payloads = Vec::new();
        let mut offsets = Vec::new();
        let mut pos = WADINFO_SIZE;
        for (_, p) in &pics {
            offsets.push(pos);
            payloads.extend_from_slice(p);
            pos += p.len();
        }
        let infotableofs = pos;

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"WAD2");
        bytes.extend_from_slice(&(pics.len() as i32).to_le_bytes());
        bytes.extend_from_slice(&(infotableofs as i32).to_le_bytes());
        bytes.extend_from_slice(&payloads);

        let mut dir = Vec::new();
        for ((name, p), &off) in pics.iter().zip(offsets.iter()) {
            push_lump(&mut dir, off as i32, p.len() as i32, name);
        }
        bytes.extend_from_slice(&dir);
        debug_assert_eq!(bytes.len(), infotableofs + pics.len() * LUMPINFO_SIZE);

        Wad2::parse(bytes).expect("synthetic gfx.wad parses")
    }

    #[test]
    fn blit_qpic_respects_transparency_and_clips() {
        let pal = ramp_palette();
        let mut img = Image::new(8, 8, [0, 0, 0]);

        // A 3x3 pic: corners opaque (index 5), centre transparent (255), with a
        // distinct opaque edge (index 9) we can detect after clipping.
        let mut data = vec![5u8; 9];
        data[3 + 1] = HUD_TRANSPARENT; // centre (row 1, col 1) transparent
        data[2 * 3 + 2] = 9; // bottom-right (row 2, col 2) opaque, distinct
        let pic = Qpic { width: 3, height: 3, data };

        // scale 1, no vertical offset (vy_top = 0), placed at virtual (0,0).
        blit_qpic(&mut img, &pic, 0.0, 0.0, 1.0, 0.0, &pal);

        // The centre texel was transparent: the background pixel is untouched.
        assert_eq!(img.rgb[8 + 1], [0, 0, 0], "index-255 texel left bg unchanged");
        // An opaque corner drew palette index 5 -> [5,5,5].
        assert_eq!(img.rgb[0], [5, 5, 5], "opaque corner blitted");
        // The distinct bottom-right opaque texel drew index 9.
        assert_eq!(img.rgb[2 * 8 + 2], [9, 9, 9], "distinct opaque texel blitted");

        // Clipping: blit the same pic so it overhangs the right/bottom edges. The
        // texels that fall off-screen must be silently dropped (no panic), and the
        // on-screen part must still draw.
        let mut img2 = Image::new(8, 8, [0, 0, 0]);
        // Place top-left at virtual (7,7): only the (0,0) texel is on-screen.
        blit_qpic(&mut img2, &pic, 7.0, 7.0, 1.0, 0.0, &pal);
        assert_eq!(img2.rgb[7 * 8 + 7], [5, 5, 5], "on-screen overhang texel drew");
        // Nothing wrapped to row 0 / col 0 from the off-screen part.
        let drawn = img2.rgb.iter().filter(|p| **p != [0, 0, 0]).count();
        assert_eq!(drawn, 1, "only the single on-screen overhang texel drew");
    }

    #[test]
    fn draw_num_right_justifies() {
        let wad = build_hud_wad();
        let pal = ramp_palette();

        // Right edge at virtual x=72 (3 * 24px digits), vy_top=0, scale=1.
        // num pics are 24x24. A 3-digit value (e.g. 100) fills [0,72); the digit
        // region (x in [0,72), y in [0,24)) must have changed.
        let mut img3 = Image::new(80, 24, [0, 0, 0]);
        draw_num(&mut img3, 100, 72.0, 0.0, 1.0, 0.0, &wad, &pal, false);
        let changed_3: usize = (0..24)
            .flat_map(|y| (0..72).map(move |x| (x, y)))
            .filter(|&(x, y)| img3.rgb[y * 80 + x] != [0, 0, 0])
            .count();
        assert!(changed_3 > 0, "3-digit value changed pixels in the digit region");

        // A 1-digit value at the same right edge must occupy only the rightmost
        // 24px slot [48,72) and leave the left two slots [0,48) untouched, proving
        // right-justification (the units digit lands at the same right edge).
        let mut img1 = Image::new(80, 24, [0, 0, 0]);
        draw_num(&mut img1, 7, 72.0, 0.0, 1.0, 0.0, &wad, &pal, false);
        // Right slot [48,72) changed.
        let right_changed: usize = (0..24)
            .flat_map(|y| (48..72).map(move |x| (x, y)))
            .filter(|&(x, y)| img1.rgb[y * 80 + x] != [0, 0, 0])
            .count();
        assert!(right_changed > 0, "1-digit value drew in the rightmost slot");
        // Left two slots [0,48) untouched.
        let left_changed: usize = (0..24)
            .flat_map(|y| (0..48).map(move |x| (x, y)))
            .filter(|&(x, y)| img1.rgb[y * 80 + x] != [0, 0, 0])
            .count();
        assert_eq!(left_changed, 0, "1-digit value left the left slots blank (right-justified)");

        // Alignment at the right edge: the units digit of "7" and the units digit
        // of "100" occupy the same column band [48,72). Both should have drawn
        // there (num_7 = index 107, num_0 = index 100 — both non-transparent).
        let units_7: usize = (0..24)
            .flat_map(|y| (48..72).map(move |x| (x, y)))
            .filter(|&(x, y)| img1.rgb[y * 80 + x] != [0, 0, 0])
            .count();
        let units_100: usize = (0..24)
            .flat_map(|y| (48..72).map(move |x| (x, y)))
            .filter(|&(x, y)| img3.rgb[y * 80 + x] != [0, 0, 0])
            .count();
        assert_eq!(units_7, units_100, "units digit of 1- and 3-digit values align at the right edge");
    }

    #[test]
    fn draw_num_right_justifies_at_scale_2() {
        // Regression for the virtual/pixel unit-mix bug: at scale != 1 the digit
        // must land at PIXEL (vx*scale), not pixel vx. Right edge virtual x=72,
        // scale=2 => the units digit must end at pixel 144 (its 24-virtual = 48-px
        // cell spans px [96,144)), and nothing draws at/after px 144.
        let wad = build_hud_wad();
        let pal = ramp_palette();
        let mut img = Image::new(200, 48, [0, 0, 0]);
        draw_num(&mut img, 7, 72.0, 0.0, 2.0, 0.0, &wad, &pal, false);

        // Pixels exist in the cell [96,144); none at or past 144.
        let in_cell = (0..48)
            .flat_map(|y| (96..144).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 200 + x] != [0, 0, 0])
            .count();
        assert!(in_cell > 0, "scale=2 digit drew in the px[96,144) cell ending at the scaled right edge");
        let past_edge = (0..48)
            .flat_map(|y| (144..200).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 200 + x] != [0, 0, 0])
            .count();
        assert_eq!(past_edge, 0, "nothing drew past the scaled right edge px=144");
        // And it must NOT be jammed against px=72 (the old bug placed it there).
        let at_virtual_edge = (0..48)
            .flat_map(|y| (48..96).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 200 + x] != [0, 0, 0])
            .count();
        assert_eq!(at_virtual_edge, 0, "digit must not sit at the unscaled px=72 edge (the old unit-mix bug)");
    }

    #[test]
    fn draw_hud_changes_bottom_strip_only() {
        let wad = build_hud_wad();
        let pal = ramp_palette();

        // A solid-filled image; the HUD must change the bottom 24-virtual-row bar
        // but leave the top of the frame untouched. Use a 320-wide frame so
        // scale == 1 and the bar is exactly the bottom 24 rows.
        let fill = [42u8, 42, 42];
        let mut img = Image::new(320, 200, fill);
        let hud = Hud { wad: &wad, palette: &pal, health: 100, ammo: 25, armor: 50 };
        draw_hud_into(&mut img, &hud);

        // The top of the frame (well above the 24-px bar) is untouched.
        for y in 0..(200 - 24) {
            for x in 0..320 {
                assert_eq!(img.rgb[y * 320 + x], fill, "row {y} col {x} above the bar must be untouched");
            }
        }
        // The bottom strip changed (the sbar background, index 1 -> [1,1,1],
        // covers the whole 320x24 bar).
        let changed_bottom: usize = (200 - 24..200)
            .flat_map(|y| (0..320).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 320 + x] != fill)
            .count();
        assert!(changed_bottom > 0, "the bottom strip changed under the HUD");

        // The digits (index >= 100) drew on top of the sbar background somewhere
        // in the bar — proving health/ammo/armour numbers actually rendered.
        let has_digit = (200 - 24..200)
            .flat_map(|y| (0..320).map(move |x| (x, y)))
            .any(|(x, y)| img.rgb[y * 320 + x][0] >= 100);
        assert!(has_digit, "at least one big-number digit drew over the bar");
    }

    #[test]
    fn draw_hud_scales_to_wide_frame() {
        // A 640-wide frame (scale 2): the bar must still bottom-anchor and span
        // the full width without panicking, leaving the top untouched.
        let wad = build_hud_wad();
        let pal = ramp_palette();
        let fill = [7u8, 7, 7];
        let mut img = Image::new(640, 400, fill);
        let hud = Hud { wad: &wad, palette: &pal, health: 99, ammo: 100, armor: 0 };
        draw_hud_into(&mut img, &hud);

        // Bar height in pixels = 24 * (640/320) = 48; the top must be untouched.
        let bar_px = (24.0 * (640.0 / 320.0)) as usize; // 48
        for y in 0..(400 - bar_px) {
            assert_eq!(img.rgb[y * 640], fill, "row {y} above the scaled bar untouched");
        }
        // Bottom row changed across a wide span (the scaled sbar covers it).
        let bottom = 399 * 640;
        let bottom_changed = (0..640).filter(|&x| img.rgb[bottom + x] != fill).count();
        assert!(bottom_changed > 320, "scaled sbar spans most of the 640-wide bottom row");
    }

    #[test]
    fn draw_hud_missing_pics_is_noop_not_panic() {
        // An empty WAD (no sbar/num pics) must degrade gracefully: the frame is
        // returned unchanged, no panic.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"WAD2");
        bytes.extend_from_slice(&0i32.to_le_bytes()); // numlumps
        bytes.extend_from_slice(&(WADINFO_SIZE as i32).to_le_bytes());
        let wad = Wad2::parse(bytes).expect("empty wad parses");
        let pal = ramp_palette();
        let fill = [9u8, 9, 9];
        let mut img = Image::new(320, 200, fill);
        let hud = Hud { wad: &wad, palette: &pal, health: 100, ammo: 50, armor: 25 };
        draw_hud_into(&mut img, &hud);
        assert!(img.rgb.iter().all(|&p| p == fill), "missing pics leave the frame unchanged");
    }

    // -- Engine particles (draw_particles: projection + z-test) ---------------

    #[test]
    fn draw_particles_in_front_changes_a_pixel() {
        // A camera at the origin looking down +X; a particle 100 units straight
        // ahead must project near screen centre and paint its palette colour over
        // a fresh (cleared) z-buffer.
        let w = 80usize;
        let h = 60usize;
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, fov_deg: 90.0 };
        let bg = [9u8, 9, 9];
        let mut img = Image::new(w, h, bg);
        let mut zbuf = vec![f32::INFINITY; w * h];
        let mut pal = [[0u8, 0, 0]; 256];
        pal[42] = [200, 50, 30]; // the particle colour

        draw_particles(&mut img, &mut zbuf, &cam, &[([100.0, 0.0, 0.0], 42)], &pal, w, h);

        // Some pixel changed to the particle colour, and the matching z-buffer
        // slot now holds the particle's forward depth (~100), not +inf.
        let painted = img.rgb.iter().filter(|&&p| p == [200, 50, 30]).count();
        assert!(painted > 0, "a particle in front must paint at least one pixel");
        let nearest = zbuf.iter().cloned().fold(f32::INFINITY, f32::min);
        assert!((nearest - 100.0).abs() < 1.0, "z-buffer holds the particle depth, got {nearest}");
    }

    #[test]
    fn draw_particles_behind_wall_is_z_tested_out() {
        // Same view, but pre-fill the z-buffer with a NEARER depth (a wall at
        // depth 10) everywhere. A particle at depth 100 is behind it and must NOT
        // be drawn (the z-test rejects vz >= zbuf).
        let w = 80usize;
        let h = 60usize;
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, fov_deg: 90.0 };
        let bg = [9u8, 9, 9];
        let mut img = Image::new(w, h, bg);
        let mut zbuf = vec![10.0f32; w * h]; // a wall closer than the particle
        let mut pal = [[0u8, 0, 0]; 256];
        pal[42] = [200, 50, 30];

        draw_particles(&mut img, &mut zbuf, &cam, &[([100.0, 0.0, 0.0], 42)], &pal, w, h);

        // Nothing painted: the wall occludes the particle.
        assert!(
            img.rgb.iter().all(|&p| p == bg),
            "a particle behind a nearer wall must be z-tested out (not drawn)"
        );
        // And the z-buffer is unchanged (still the wall depth).
        assert!(zbuf.iter().all(|&z| z == 10.0), "occluded particle must not overwrite the z-buffer");
    }

    #[test]
    fn draw_particles_in_front_overwrites_farther_wall() {
        // A particle CLOSER than the existing z-buffer (a wall at depth 500) must
        // win the z-test and paint, writing its own depth — the complement of the
        // occlusion test above.
        let w = 80usize;
        let h = 60usize;
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, fov_deg: 90.0 };
        let mut img = Image::new(w, h, [9, 9, 9]);
        let mut zbuf = vec![500.0f32; w * h]; // a wall FARTHER than the particle
        let mut pal = [[0u8, 0, 0]; 256];
        pal[7] = [10, 220, 40];

        draw_particles(&mut img, &mut zbuf, &cam, &[([100.0, 0.0, 0.0], 7)], &pal, w, h);

        let painted = img.rgb.iter().filter(|&&p| p == [10, 220, 40]).count();
        assert!(painted > 0, "a particle nearer than the wall must paint");
        let nearest = zbuf.iter().cloned().fold(f32::INFINITY, f32::min);
        assert!((nearest - 100.0).abs() < 1.0, "nearer particle writes its depth, got {nearest}");
    }

    #[test]
    fn draw_particles_behind_camera_is_skipped() {
        // A particle at/behind the near plane (here directly behind the camera)
        // must be skipped entirely — no panic, no paint.
        let w = 40usize;
        let h = 30usize;
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, fov_deg: 90.0 };
        let bg = [9u8, 9, 9];
        let mut img = Image::new(w, h, bg);
        let mut zbuf = vec![f32::INFINITY; w * h];
        let pal = [[200u8, 200, 200]; 256];

        // -X is behind a camera looking down +X.
        draw_particles(&mut img, &mut zbuf, &cam, &[([-100.0, 0.0, 0.0], 0)], &pal, w, h);
        assert!(img.rgb.iter().all(|&p| p == bg), "a particle behind the camera draws nothing");
    }

    #[test]
    fn render_scene_empty_particles_matches_no_particles() {
        // Passing an empty particle slice to render_scene_ext must reproduce the
        // exact frame render_scene produces (no particles == no change).
        let bsp = demo_room();
        let cam = Camera::looking_at([0.0, 0.0, 0.0], [200.0, 0.0, 0.0], 90.0);
        let pal = [[180u8, 180, 180]; 256];
        let with_empty = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES);
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
        let without = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES);
        // A particle ~80 units in front of the camera (well before the +256 wall).
        let with = render_scene_ext(
            &bsp,
            &cam,
            160,
            120,
            &pal,
            &[],
            &[],
            None,
            0.0,
            &[([-120.0, 0.0, 0.0], 251)],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
        );
        assert_ne!(without.rgb, with.rgb, "a visible particle must change the frame");
        assert!(
            with.rgb.iter().any(|&p| p == [255, 0, 255]),
            "the particle's palette colour must appear in the frame"
        );
    }

    // -- main menu (Menu engine + draw_menu + draw_string) ------------------

    /// A solid `w*h` Qpic filled with palette index `idx`.
    fn solid_pic(w: i32, h: i32, idx: u8) -> crate::wad::Qpic {
        crate::wad::Qpic {
            width: w,
            height: h,
            data: vec![idx; (w * h) as usize],
        }
    }

    #[test]
    fn menu_move_cursor_wraps_within_each_screen() {
        let mut m = Menu::new();
        m.open(); // Main: 5 items.
        assert_eq!(m.screen(), MenuScreen::Main);
        assert_eq!(m.cursor(), 0);
        // Down past the end wraps to 0.
        for expect in [1, 2, 3, 4, 0, 1] {
            m.move_cursor(1);
            assert_eq!(m.cursor(), expect);
        }
        // Up below 0 wraps to the last item (4).
        m.move_cursor(-1);
        assert_eq!(m.cursor(), 0);
        m.move_cursor(-1);
        assert_eq!(m.cursor(), 4);

        // On the single-player screen the wrap is modulo 3.
        m.cursor = 0;
        let action = m.select(); // Main>Single Player
        assert_eq!(action, MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::SinglePlayer);
        for expect in [1, 2, 0, 1] {
            m.move_cursor(1);
            assert_eq!(m.cursor(), expect);
        }
        // A large delta still wraps correctly.
        m.cursor = 0;
        m.move_cursor(7); // 7 % 3 = 1
        assert_eq!(m.cursor(), 1);
        m.move_cursor(-7); // back to 0
        assert_eq!(m.cursor(), 0);
    }

    #[test]
    fn menu_select_and_cancel_transitions() {
        let mut m = Menu::new();
        m.open();

        // Main > Single Player goes to the submenu, no host action.
        m.cursor = 0;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::SinglePlayer);
        assert!(m.visible);

        // SinglePlayer > New Game returns NewGame and closes the menu.
        m.cursor = 0;
        assert_eq!(m.select(), MenuAction::NewGame);
        assert!(!m.visible);

        // Re-open: Escape on a submenu goes Back to Main (still visible).
        m.open();
        m.select(); // -> SinglePlayer
        assert_eq!(m.screen(), MenuScreen::SinglePlayer);
        assert_eq!(m.cancel(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Main);
        assert!(m.visible);

        // Escape on Main closes the menu.
        assert_eq!(m.cancel(), MenuAction::Closed);
        assert!(!m.visible);

        // Cancel on a hidden menu is a no-op.
        assert_eq!(m.cancel(), MenuAction::None);

        // Quit (item 4 on Main) closes the menu.
        m.open();
        m.cursor = 4;
        assert_eq!(m.select(), MenuAction::Closed);
        assert!(!m.visible);

        // Unimplemented Main items (Multiplayer/Options/Help) do nothing.
        m.open();
        for c in [1usize, 2, 3] {
            m.cursor = c;
            assert_eq!(m.select(), MenuAction::None);
            assert_eq!(m.screen(), MenuScreen::Main, "item {c} must not change screen");
            assert!(m.visible);
        }
    }

    #[test]
    fn menu_toggle_open_back_close() {
        let mut m = Menu::new();
        // Hidden -> open on Main.
        assert_eq!(m.toggle(), MenuAction::None);
        assert!(m.visible);
        assert_eq!(m.screen(), MenuScreen::Main);
        // On a submenu, toggle backs out to Main.
        m.select(); // Main>SinglePlayer
        assert_eq!(m.toggle(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Main);
        assert!(m.visible);
        // On Main, toggle closes.
        assert_eq!(m.toggle(), MenuAction::Closed);
        assert!(!m.visible);
    }

    #[test]
    fn draw_menu_skips_missing_pics_without_panic() {
        let pal = ramp_palette();
        let mut img = Image::new(320, 200, [9, 9, 9]);
        let before = img.rgb.clone();
        let mut m = Menu::new();
        m.open();
        // All pics absent: nothing should draw, and it must not panic.
        let pics = MenuPics::default();
        draw_menu(&mut img, &m, &pics, None, 0.3, &pal);
        assert_eq!(img.rgb, before, "an all-empty MenuPics must leave the frame untouched");

        // A hidden menu never draws.
        m.close();
        let solid = solid_pic(64, 16, 7);
        let pics2 = MenuPics { mainmenu: Some(solid), ..Default::default() };
        draw_menu(&mut img, &m, &pics2, None, 0.3, &pal);
        assert_eq!(img.rgb, before, "a hidden menu must not draw");
    }

    #[test]
    fn draw_menu_draws_present_pics_over_background() {
        let pal = ramp_palette();
        let mut img = Image::new(320, 200, [9, 9, 9]);
        let mut m = Menu::new();
        m.open();
        // A present mainmenu graphic (opaque index 7 -> a non-background colour)
        // at (72,32) must change pixels there.
        let pics = MenuPics {
            mainmenu: Some(solid_pic(120, 80, 7)),
            ..Default::default()
        };
        draw_menu(&mut img, &m, &pics, None, 0.0, &pal);
        // At scale 1 on the 320x200 frame, virtual (72,32) maps to pixel (72,32).
        let idx = 32 * img.w + 72;
        assert_eq!(img.rgb[idx], pal[7], "the mainmenu pic must paint at (72,32)");
        assert_ne!(img.rgb[idx], [9, 9, 9], "the pixel must differ from the background");
        // A corner well outside the pic stays background.
        assert_eq!(img.rgb[0], [9, 9, 9]);
    }

    #[test]
    fn draw_menu_cursor_frame_animates_with_time() {
        let pal = ramp_palette();
        let mut m = Menu::new();
        m.open();
        // Distinct colours per cursor frame so we can detect which frame drew.
        let mut menudot: [Option<crate::wad::Qpic>; 6] = Default::default();
        for (i, slot) in menudot.iter_mut().enumerate() {
            *slot = Some(solid_pic(20, 20, 10 + i as u8));
        }
        let pics = MenuPics { menudot, ..Default::default() };

        // The cursor sits at (54, 32). frame = (time*10) % 6.
        let cursor_idx = 32 * 320 + 54;
        let mut img0 = Image::new(320, 200, [0, 0, 0]);
        draw_menu(&mut img0, &m, &pics, None, 0.0, &pal); // frame 0 -> index 10
        assert_eq!(img0.rgb[cursor_idx], pal[10]);

        let mut img1 = Image::new(320, 200, [0, 0, 0]);
        draw_menu(&mut img1, &m, &pics, None, 0.35, &pal); // (3.5)->3 -> index 13
        assert_eq!(img1.rgb[cursor_idx], pal[13]);
    }

    #[test]
    fn draw_string_writes_glyph_pixels() {
        let pal = ramp_palette();
        // A 128x128 conchars where every texel is the lit index 3 EXCEPT the
        // space cell (byte 32 -> cell (0,2)) which stays at the transparent 0.
        // With an all-lit atlas, any non-space character stamps index-3 pixels.
        let mut data = vec![3u8; 128 * 128];
        // Zero out the byte-0 cell (top-left 8x8) so it's transparent.
        for y in 0..8 {
            for x in 0..8 {
                data[y * 128 + x] = 0;
            }
        }
        let conchars = crate::wad::Qpic { width: 128, height: 128, data };

        let mut img = Image::new(64, 16, [0, 0, 0]);
        draw_string(&mut img, &conchars, 0, 0, "A", &pal);
        // 'A' = byte 65 = cell (1, 4): source (8, 32). Its texels are lit (index 3),
        // stamped at the destination starting (0,0). So pixel (0,0) is index 3.
        assert_eq!(img.rgb[0], pal[3], "the glyph's lit texel must paint");
        // A space draws nothing past the first char; draw a string and confirm the
        // second char ('B') lands 8 px to the right.
        let mut img2 = Image::new(64, 16, [0, 0, 0]);
        draw_string(&mut img2, &conchars, 0, 0, " B", &pal);
        // Space is skipped, 'B' starts at virtual x=8.
        assert_eq!(img2.rgb[8], pal[3], "the second glyph must land 8px right");
        assert_eq!(img2.rgb[0], [0, 0, 0], "a leading space must draw nothing");
    }
}
