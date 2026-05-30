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

/// A face's baked static lightmap (style 0 only), borrowed from `Bsp::lighting`.
///
/// `samples` is a `lmw * lmh` grid of one-byte luxels. `texmins` is the surface
/// texture-coordinate origin (in texels) used to convert a face's surface `(s,t)`
/// into luxel coordinates. The lightmap shares the face's `texinfo.vecs` with the
/// wall texture, so the same surface `(s,t)` the rasteriser already interpolates
/// indexes both.
struct LightMap<'a> {
    samples: &'a [u8],
    lmw: usize,
    lmh: usize,
    texmins: [f32; 2],
}

impl LightMap<'_> {
    /// Bilinearly sample the lightmap at surface texture coordinate `(s, t)`,
    /// returning a brightness factor (1.0 == neutral, up to ~2.0 overbright).
    ///
    /// Luxel indices are clamped into `[0, lm-1]`. Any index that somehow falls
    /// outside the sample slice is treated as the fullbright luxel value 255, so
    /// a malformed lightmap never reads garbage and never panics.
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

        // Treat any out-of-slice luxel as fullbright (255).
        let at = |x: usize, y: usize| -> f32 {
            self.samples.get(y * self.lmw + x).copied().unwrap_or(255) as f32
        };

        let top = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
        let bot = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
        let light = top * (1.0 - fy) + bot * fy;

        (light / 255.0) * 2.0
    }
}

/// Compute a face's static lightmap, or `None` if the face is fullbright.
///
/// A face is fullbright when there is no `lighting` lump, the face has no
/// lightmap (`lightofs < 0`), the surface is special (sky/liquid — `TEX_SPECIAL`),
/// or the computed luxel grid would not fit in the remaining `lighting` slice.
/// Only the base lightmap (style 0) is applied; animated styles `[1..3]` are
/// ignored.
fn face_lightmap<'a>(
    bsp: &'a Bsp,
    face: &crate::bsp::DFace,
    world_poly: &[Vec3],
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
    let samples = bsp.lighting.get(start..start.checked_add(count)?)?;

    Some(LightMap {
        samples,
        lmw,
        lmh,
        texmins: [texmins[0] as f32, texmins[1] as f32],
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
            let tx = (s as i64).rem_euclid(tw as i64) as usize;
            let ty = (t as i64).rem_euclid(th as i64) as usize;
            let texel = match pixels.get(ty * tw + tx) {
                Some(&p) => p as usize,
                None => continue,
            };
            let rgb = palette[texel];
            // Brightness: a baked lightmap replaces the flat Lambert `shade`.
            // The lightmap is indexed by the same surface `(s,t)` (it shares the
            // texinfo axes with the wall texture). Fullbright faces keep `shade`.
            let brightness = match lightmap {
                Some(lm) => lm.factor_at(s, t),
                None => shade,
            };
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
    draw_world_textured(&mut image, &mut zbuf, bsp, cam, palette);
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
fn draw_world_textured(
    image: &mut Image,
    zbuf: &mut [f32],
    bsp: &Bsp,
    cam: &Camera,
    palette: &[[u8; 3]; 256],
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

        // Baked static lightmap for this face (None => fullbright). Computed once
        // from the world polygon; passed down to every fan triangle.
        let lightmap = face_lightmap(bsp, face, &world_poly);

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
                        &mt.pixels, tw, th, palette, shade, lightmap.as_ref(),
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
                                &one, 1, 1, &pal1, shade, Some(&lm),
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
fn draw_submodel(
    image: &mut Image,
    zbuf: &mut [f32],
    bsp: &Bsp,
    cam: &Camera,
    palette: &[[u8; 3]; 256],
    model_index: usize,
    origin: Vec3,
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

        // Lightmap from the LOCAL polygon (texinfo extents are origin-independent).
        let lightmap = face_lightmap(bsp, face, &local_poly);

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
                        &mt.pixels, tw, th, palette, shade, lightmap.as_ref(),
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
                                &one, 1, 1, &pal1, shade, Some(&lm),
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

/// Render `bsp` with its real miptextures (as [`render_bsp_textured`]) and then
/// draw each alias-model `instances` entry into the same image, sharing one
/// z-buffer so models and world occlude one another correctly.
///
/// A thin wrapper over [`render_scene_ext`] with no brush submodels; kept as the
/// stable entry point the binary and wasm front-ends call.
pub fn render_scene(
    bsp: &Bsp,
    cam: &Camera,
    w: usize,
    h: usize,
    palette: &[[u8; 3]; 256],
    instances: &[ModelInstance],
) -> Image {
    render_scene_ext(bsp, cam, w, h, palette, instances, &[])
}

/// Render the full scene: the textured world, then each brush submodel
/// (`bmodels`), then each alias model (`models`), all sharing one z-buffer so
/// every piece occludes — and is occluded by — the others correctly.
///
/// Brush submodels are drawn *before* alias models, matching `render_scene`'s
/// world-then-models ordering; correctness does not depend on the order because
/// the shared depth buffer resolves visibility per pixel. Passing an empty
/// `bmodels` slice reproduces [`render_scene`] exactly (the binary/wasm path).
pub fn render_scene_ext(
    bsp: &Bsp,
    cam: &Camera,
    w: usize,
    h: usize,
    palette: &[[u8; 3]; 256],
    models: &[ModelInstance],
    bmodels: &[BModelInstance],
) -> Image {
    let mut image = Image::new(w, h, [10, 10, 14]);
    if w == 0 || h == 0 {
        return image;
    }
    let mut zbuf = vec![f32::INFINITY; w.saturating_mul(h)];
    draw_world_textured(&mut image, &mut zbuf, bsp, cam, palette);
    for bm in bmodels {
        draw_submodel(&mut image, &mut zbuf, bsp, cam, palette, bm.model_index, bm.origin);
    }
    for inst in models {
        draw_alias_model(&mut image, &mut zbuf, cam, inst, w, h, palette);
    }
    image
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
            samples: &samples,
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
            samples: &samples,
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
        assert_eq!(lm.samples.len(), 9);
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

        let without = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[]);
        let with = render_scene_ext(
            &bsp,
            &cam,
            160,
            120,
            &pal,
            &[],
            // Place the quad between the camera (-200) and the centre, facing it.
            &[BModelInstance { model_index: 1, origin: [-120.0, 0.0, 0.0] }],
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

        let empty = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[]);
        let oob = render_scene_ext(
            &bsp,
            &cam,
            160,
            120,
            &pal,
            &[],
            &[BModelInstance { model_index: 999, origin: [-120.0, 0.0, 0.0] }],
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
        let b = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[]);
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
        );
    }
}
