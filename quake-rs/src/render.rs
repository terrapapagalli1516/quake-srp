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

/// `V_CalcPowerupCshift` (view.c): the full-screen tint while a powerup is held —
/// Quad=blue, Biosuit=green, Ring(invisibility)=gray, Pentagram(invulnerability)=
/// yellow — as `(rgb, percent)`, or `None` with no powerup. id uses an `else if`
/// chain, so the FIRST match wins with priority QUAD > SUIT > INVISIBILITY >
/// INVULNERABILITY (a combined Quad+Pentagram shows the Quad's blue tint).
pub fn powerup_cshift(items: i32) -> Option<([u8; 3], f32)> {
    if items & IT_QUAD != 0 {
        Some(([0, 0, 255], 30.0))
    } else if items & IT_SUIT != 0 {
        Some(([0, 255, 0], 20.0))
    } else if items & IT_INVISIBILITY != 0 {
        Some(([100, 100, 100], 100.0))
    } else if items & IT_INVULNERABILITY != 0 {
        Some(([255, 255, 0], 30.0))
    } else {
        None
    }
}

/// Combine colour shifts `(rgb, percent 0..255)` into a single blend colour and
/// alpha (0..1), porting Quake's `V_CalcBlend` accumulation (each shift is
/// alpha-over the running total). Empty list / all-zero percents give alpha 0.
// `!(percent > 0.0)` is deliberate (a hardened V_CalcBlend skip): it also skips a
// NaN percent, which the clippy-suggested `percent <= 0.0` would let through.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
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
/// Software Quake's `V_UpdatePalette` runs LAST in `SCR_UpdateScreen` and shifts the
/// whole VGA palette, so the tint covers the ENTIRE composited screen — 3-D view,
/// status bar, centerprint, menu and console alike. Apply this to the FINISHED frame
/// after every overlay, not just the 3-D viewport (which would be the GL look).
// `!(alpha > 0.0)` is deliberate: a NaN alpha must also be a no-op, which the
// clippy-suggested `alpha <= 0.0` would not guarantee.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
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

/// `D_WarpScreen` (d_scan.c): the underwater full-screen sine wobble applied when
/// the view leaf is in water/slime/lava (`r_waterwarp`, default on). Each output
/// pixel samples a source pixel displaced by a per-row/per-column sine offset
/// (`AMP2 = 3`, `SPEED = 20`, 128-cycle `intsintable`), with a slight edge
/// compression (`dim/(dim + 2*AMP2)`) so the warp never reads outside the frame.
/// The row displacement is driven by the column's sine and vice-versa (the classic
/// cross-coupled warp). Operates on a snapshot of the frame; `clock` drives the
/// phase. Applied to the 3-D frame BEFORE the content tint (V_SetContentsColor),
/// so wobble and tint compose exactly as in stock software Quake.
// The `3.14159` below is id's truncated literal (see the in-body comment): using
// `std::f64::consts::PI` would shift the table by one index and break the warp's
// byte-identity, so the clippy::approx_constant lint is deliberately allowed here.
#[allow(clippy::approx_constant)]
pub fn apply_warp(image: &mut Image, clock: f32) {
    const AMP2: i32 = 3;
    const SPEED: f64 = 20.0;
    let w = image.w as i32;
    let h = image.h as i32;
    if w <= 0 || h <= 0 {
        return;
    }
    // intsintable[i] = (int)(AMP2 + AMP2*sin(i*3.14159*2/128)) — truncated, 0..2*AMP2.
    // id's R_InitTurb uses the truncated literal 3.14159 (NOT exact pi), so the table
    // tops out at 5 (a broad plateau), never 6: at i=32 the argument falls just short
    // of pi/2 so sin<1 and (int)5.999..=5. Using exact 2*pi would give 6 at i=32 — a
    // one-index divergence. Match the C literal for bit-identical warp.
    let mut sintable = [0i32; 128];
    for (i, s) in sintable.iter_mut().enumerate() {
        let f = AMP2 as f64 + AMP2 as f64 * ((i as f64) * 3.14159 * 2.0 / 128.0).sin();
        *s = f as i32; // (int) truncation, matching the C table build
    }
    // rowptr[i] = compressed source row for stretched index i in 0..h+2*AMP2.
    let rspan = (h + 2 * AMP2) as usize;
    let mut rowptr = vec![0usize; rspan];
    for (i, r) in rowptr.iter_mut().enumerate() {
        let v = (i as i64 * h as i64 / (h + 2 * AMP2) as i64) as i32;
        *r = v.clamp(0, h - 1) as usize;
    }
    // column[j] = compressed source column for stretched index j in 0..w+2*AMP2.
    let cspan = (w + 2 * AMP2) as usize;
    let mut column = vec![0usize; cspan];
    for (j, c) in column.iter_mut().enumerate() {
        let u = (j as i64 * w as i64 / (w + 2 * AMP2) as i64) as i32;
        *c = u.clamp(0, w - 1) as usize;
    }
    let phase = ((clock as f64 * SPEED) as i64 & 127) as usize;
    let src = image.rgb.clone(); // pre-warp snapshot
    let (wu, hu) = (w as usize, h as usize);
    for v in 0..hu {
        let tv = sintable[(phase + v) & 127] as usize; // 0..2*AMP2
        for u in 0..wu {
            let tu = sintable[(phase + u) & 127] as usize; // 0..2*AMP2
            let src_row = rowptr[v + tu];
            let src_col = column[tv + u];
            image.rgb[v * wu + u] = src[src_row * wu + src_col];
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
    /// View bank in degrees about the forward axis (Quake's `viewangles[ROLL]` from
    /// V_CalcViewRoll: strafe lean, damage kick, and the 80° dead-view tilt). `0.0`
    /// keeps the horizon level and reproduces the pre-roll basis bit-for-bit.
    pub roll: f32,
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
            roll: 0.0,
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
        if self.roll == 0.0 {
            // No bank: exact pre-roll basis (keeps level-view renders bit-identical).
            return (forward, right, up);
        }
        // Bank the (right, up) pair about the forward axis by `roll` degrees. Derived
        // from id's AngleVectors at pitch=0: with sr=sin(roll), cr=cos(roll),
        //   right' = cr*right - sr*up,  up' = sr*right + cr*up.
        // Rotating the already-pitch-correct level basis about forward reproduces
        // V_CalcViewRoll's bank at any pitch.
        let rr = (self.roll as f64).to_radians();
        let (sr, cr) = (rr.sin(), rr.cos());
        let right2: Vec3 = [
            (right[0] as f64 * cr - up[0] as f64 * sr) as f32,
            (right[1] as f64 * cr - up[1] as f64 * sr) as f32,
            (right[2] as f64 * cr - up[2] as f64 * sr) as f32,
        ];
        let up2: Vec3 = [
            (right[0] as f64 * sr + up[0] as f64 * cr) as f32,
            (right[1] as f64 * sr + up[1] as f64 * cr) as f32,
            (right[2] as f64 * sr + up[2] as f64 * cr) as f32,
        ];
        (forward, right2, up2)
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
    // FAITHFULNESS (perspective-correct depth): Quake's span renderer keyed the
    // z-buffer on `1/z` (`d_scan.c`/`d_edge.c` interpolate `zi`), because `1/z`
    // is linear in screen space while view-`z` is NOT — so a linear `vz`
    // interpolation mis-sorts intersecting polys. We interpolate `1/z` (exact in
    // screen space) and recover the true per-pixel depth `z = 1/(Σ wᵢ/zᵢ)`. That
    // keeps the existing "smaller depth = nearer" convention (and the `INFINITY`
    // init / `<` tests everywhere) intact while making the test perspective
    // correct. A vertex with non-positive depth is guarded below.
    let (iz0, iz1, iz2) = (1.0 / v0.depth, 1.0 / v1.depth, 1.0 / v2.depth);

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

            // Perspective-correct depth: interpolate 1/z (linear in screen space)
            // and invert. `inv_z <= 0` means a vertex was at/behind the eye; skip.
            let inv_z = w0 * iz0 + w1 * iz1 + w2 * iz2;
            if inv_z <= 0.0 {
                continue;
            }
            let depth = 1.0 / inv_z;

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

/// Quake's `gfx/colormap.lmp` is `COLORMAP_ROWS * 256` bytes: `COLORMAP_ROWS`
/// successive light rows of 256 palette indices each. Row 0 is the brightest
/// (the base palette colour), the last row is the darkest. `VID_CBITS == 6`, so
/// `VID_GRADES == 64` rows.
const COLORMAP_ROWS: usize = 64; // 1 << VID_CBITS, VID_CBITS == 6
/// A correctly-sized colormap is exactly this many bytes.
const COLORMAP_LEN: usize = COLORMAP_ROWS * 256;

/// Map a per-pixel lightmap `brightness` factor (as produced by
/// [`LightMap::factor_at`]: `1.0` neutral, `2.0` the static fullbright ceiling,
/// up to [`MAX_LIGHT_FACTOR`] with dynamic lights) to a Quake colormap ROW in
/// `0..COLORMAP_ROWS`.
///
/// C basis — `R_BuildLightMap` (`r_surf.c`) then the `D_DrawSurfaceBlock8` inner
/// loop (`r_surf.c` / `d_scan.c`):
///
/// ```c
/// // r_surf.c, R_BuildLightMap, "bound, invert, and shift":
/// t = (255*256 - (int)blocklights[i]) >> (8 - VID_CBITS);   // VID_CBITS == 6
/// if (t < (1 << 6)) t = (1 << 6);                            // clamp t >= 64
/// blocklights[i] = t;
/// // d_scan.c inner loop:
/// prowdest[b] = ((unsigned char *)vid.colormap)[(light & 0xFF00) + pix];
/// //                                              ^ row = light >> 8
/// ```
///
/// `blocklights[i]` is the combined light in 8.8 units. For the canonical static
/// single-style surface Quake scales the luxel by `d_lightstylevalue == 256`
/// (1.0 in 8.8), so `blocklights == luxel * 256`. This renderer's `brightness`
/// is `(luxel/255)*2`, hence `luxel == brightness*127.5` and
/// `blocklights == brightness * 32640` (`= 127.5 * 256`). Substituting into the
/// C: `t = (65280 - brightness*32640) >> 2`, clamped to `>= 64`, and the
/// colormap row is `t >> 8`. brightness `2.0` (and anything brighter — dynamic
/// lights) collapses to row 0 via the `t >= 64` clamp, reproducing Quake's
/// no-overbright ceiling; brightness `0.0` maps to the darkest row.
#[inline]
fn colormap_row(brightness: f32) -> usize {
    // blocklights in 8.8 units; round to match the integer `(int)blocklights`.
    let bl = (brightness * 32640.0 + 0.5).floor() as i32;
    // C: (255*256 - blocklights) >> (8 - VID_CBITS) == ... >> 2
    let mut t = (65280 - bl) >> 2;
    // C: if (t < (1<<6)) t = (1<<6);  -- the no-overbright clamp.
    if t < (1 << 6) {
        t = 1 << 6;
    }
    // C: light & 0xFF00, i.e. row = t >> 8. Clamp the row into the table; a
    // fully dark luxel (bl == 0) gives t == 16320 -> row 63, already the last
    // row, but guard against any future widening.
    ((t >> 8) as usize).min(COLORMAP_ROWS - 1)
}

/// A `surf->dlightbits` mask with every slot set: "marked by every light".
/// Used where the `R_MarkLights` recursion does not apply — direct unit calls
/// on a bare face, and the no-node-tree fallback in [`mark_dlights`] — so the
/// per-light distance test in [`add_dynamic_lights`] is the only gate (the
/// pre-gating behaviour).
const ALL_DLIGHT_BITS: u32 = u32::MAX;

/// `R_AddDynamicLights` (`r_surf.c`): fold the dynamic lights in `dlights` that
/// touch a face into an owned augmented luxel buffer.
///
/// `dlightbits` is the face's `surf->dlightbits` mask from the `R_MarkLights`
/// BSP recursion ([`mark_dlights`]): bit `i` set means `dlights[i]` reached this
/// face through the node tree. A light whose bit is clear is skipped exactly as
/// the C `if (!(surf->dlightbits & (1<<lnum))) continue;` — so a light cannot
/// brighten a face the BSP says it never reaches (e.g. through a wall). Callers
/// outside the marked render passes (unit tests on a bare face) pass
/// [`ALL_DLIGHT_BITS`] to apply the pure distance test.
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
    dlightbits: u32,
) -> Option<Vec<f32>> {
    // No dlights — or none marked for this face by the BSP recursion: the
    // animated combine (if any) is the final buffer; otherwise there is nothing
    // to do and the caller keeps the static borrow.
    if dlights.is_empty() || dlightbits == 0 {
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

    for (lnum, dl) in dlights.iter().enumerate() {
        // C: `if (!(surf->dlightbits & (1<<lnum))) continue;` — only lights the
        // R_MarkLights recursion marked onto this face apply. Lights past bit 31
        // cannot be expressed in the mask (C MAX_DLIGHTS is 32) and are skipped.
        if lnum >= u32::BITS as usize || dlightbits & (1u32 << lnum) == 0 {
            continue;
        }
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
    face_lightmap_dyn(bsp, face, world_poly, &NEUTRAL_LIGHTSTYLE_SCALES, &[], 0)
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
    // The face's `surf->dlightbits` mask from the `R_MarkLights` BSP recursion
    // (see [`mark_dlights`]); [`ALL_DLIGHT_BITS`] where marking does not apply.
    dlightbits: u32,
) -> Option<LightMap<'a>> {
    use crate::bsp::TEX_SPECIAL;

    if bsp.lighting.is_empty() {
        return None;
    }
    let ti = (face.texinfo as i64)
        .try_into()
        .ok()
        .and_then(|i: usize| bsp.texinfo.get(i))?;
    if ti.flags & TEX_SPECIAL != 0 {
        // Sky / liquid: never lightmapped, never dlit (C `SURF_DRAWTILED`).
        return None;
    }

    // FAITHFULNESS (dlight on an unlit face): a NORMAL wall with no baked lightmap
    // (`lightofs < 0`) is NOT fullbright when a dynamic light reaches it — Quake's
    // `R_BuildLightMap` clears the block to ambient and `R_AddDynamicLights` adds
    // onto it (the surface still gets a `blocklights` array). We mirror that: when
    // such a face has reaching dlights we build a ZERO base and add the lights;
    // when there are no dlights we keep returning `None` (fullbright), so the
    // common case is byte-identical to before.
    if face.lightofs < 0 {
        if dlights.is_empty() || dlightbits == 0 {
            return None;
        }
        let (texmins, extent) = surface_extents(ti, world_poly)?;
        let lmw = (extent[0] / 16 + 1) as usize;
        let lmh = (extent[1] / 16 + 1) as usize;
        let count = lmw.checked_mul(lmh)?;
        let texmins_f = [texmins[0] as f32, texmins[1] as f32];
        // Pass an EMPTY `static_samples` and a `None` base: `add_dynamic_lights`
        // lazily materialises a zero-filled buffer (the C "clear to ambient", with
        // ambient 0) ONLY when a light actually reaches this face, and returns
        // `None` otherwise. So a far-away dlight leaves the unlit face fullbright
        // (unchanged), while a reaching one dims/brightens it like the C.
        let _ = count; // the grid size is implicit in lmw*lmh inside the helper
        let no_samples: &[u8] = &[];
        let luxels = match add_dynamic_lights(
            bsp, face, ti, texmins_f, lmw, lmh, no_samples, None, dlights, dlightbits,
        ) {
            Some(owned) => Luxels::Owned(owned),
            None => return None,
        };
        return Some(LightMap { luxels, lmw, lmh, texmins: texmins_f });
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
    let luxels = match add_dynamic_lights(bsp, face, ti, texmins_f, lmw, lmh, samples, base, dlights, dlightbits) {
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

/// WinQuake `R_InitTurb` constants (r_main.c / r_local.h): the software liquid
/// warp drives a 128-entry sine table by the INTEGER texel coordinate (not a
/// scaled float coord like GLQuake's `EmitWaterPolys`), scrolled by `time*SPEED`.
const TURB_CYCLE: usize = 128;
/// `AMP` — the table swings `8 + 8*sin` texels (0..16; the +8 DC bias is wrapped
/// off by the 64-texel liquid texture downstream, exactly as `&63` in `d_scan.c`).
const TURB_AMP: f32 = 8.0;
/// `SPEED` — the table phase advances by `time*20` per second.
const TURB_SPEED: f32 = 20.0;

/// WinQuake's `sintable` (`R_InitTurb`): `sintable[i] = 8 + 8*sin(i*2pi/128)` in
/// texel units, 128-periodic. Indexed by an integer texel coordinate (the other
/// axis) plus the time phase — this is the SOFTWARE `Turbulent8` warp, which has a
/// different ripple wavelength and ~20x the scroll speed of the GL water warp.
struct TurbTable {
    tab: [i32; TURB_CYCLE],
}

impl TurbTable {
    /// Build the table once at render start (`f32::sin` is not yet `const`).
    fn new() -> TurbTable {
        let mut tab = [0i32; TURB_CYCLE];
        let mut i = 0usize;
        while i < TURB_CYCLE {
            tab[i] = (TURB_AMP
                + TURB_AMP * ((i as f32) * 2.0 * std::f32::consts::PI / (TURB_CYCLE as f32)).sin())
            .round() as i32;
            i += 1;
        }
        TurbTable { tab }
    }

    /// The texel displacement for integer index `k` (the OTHER axis' texel coord
    /// plus the time phase), masked to the 128 cycle exactly as the C `&(CYCLE-1)`.
    #[inline]
    fn at_int(&self, k: i32) -> i32 {
        self.tab[(k & (TURB_CYCLE as i32 - 1)) as usize]
    }
}

/// Apply the SOFTWARE liquid warp (`d_scan.c` `Turbulent8`) to a surface texel
/// `(s,t)` at game `time`, returning the displaced integer-texel `(s2,t2)`:
///
/// ```text
/// phase = (int)(time * SPEED)               // SPEED = 20
/// s2 = s + sintable[(t + phase) & 127]      // sintable in texels
/// t2 = t + sintable[(s + phase) & 127]
/// ```
///
/// Each axis is offset by the sine of the *other* axis' integer texel coordinate
/// plus the time phase, so the surface ripples. The caller wraps `(s2,t2)` into
/// the 64-texel liquid texture via `rem_euclid` (matching the C's final `&63`).
#[inline]
fn warp_st(turb: &TurbTable, s: f32, t: f32, time: f32) -> (f32, f32) {
    let phase = (time * TURB_SPEED) as i32;
    let si = s.floor() as i32;
    let ti = t.floor() as i32;
    let s2 = (si + turb.at_int(ti + phase)) as f32;
    let t2 = (ti + turb.at_int(si + phase)) as f32;
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

/// The camera projection a sky pixel needs to recover its world view direction,
/// porting `D_Sky_uv_To_st`'s use of `vpn`/`vright`/`vup` and the screen centre.
///
/// The sky is an infinite dome: what a screen pixel shows depends on the view
/// DIRECTION through that pixel, NOT on the wall polygon's `(s,t)`. This carries
/// the camera basis and the projection parameters so [`sky_texel_view`] can
/// rebuild the ray for each covered pixel.
#[derive(Clone, Copy)]
struct SkyView {
    forward: Vec3,
    right: Vec3,
    up: Vec3,
    /// Screen centre (`w/2`, `h/2`).
    cx: f32,
    cy: f32,
    /// `max(width, height)` — the `temp` normaliser in `D_Sky_uv_To_st`. The C
    /// derives the ray from this fixed `8192/longest` scaling (a fixed dome angle,
    /// independent of the render FOV), so the camera focal length is not used.
    longest: f32,
}

/// Sample the sky for screen pixel `(u, v)` by projecting the **view direction**
/// onto the scrolling sky, porting `D_Sky_uv_To_st` (`d_sky.c`).
///
/// `D_Sky_uv_To_st` builds the world ray for the pixel —
/// `end = 4096*vpn + wu*vright + wv*vup` with `wu`/`wv` the screen offsets scaled
/// by `8192/longest`, then `end[2] *= 3` (vertical squash) and normalise — and
/// derives the sky coords `s = scroll + 6*(SKYSIZE/2-1)*end[0]`,
/// `t = scroll + 6*(SKYSIZE/2-1)*end[1]`. The `scroll = skytime*skyspeed` drifts
/// the whole sky over time (`skyspeed = 8`). We feed those `(s,t)` to the same
/// two-layer overlay/background lookup [`sky_texel`] already implements, so the
/// front cloud layer scrolls over the solid background. The result depends only
/// on where the camera looks, so the sky no longer smears with wall coords and
/// scrolls as the player turns.
///
/// SAFETY: `focal`/`longest` are guarded against 0 by the caller; the lookup is
/// `sky_texel`, which bounds-checks and wraps, so a malformed sky never panics.
#[allow(clippy::too_many_arguments)]
#[inline]
fn sky_texel_view(pixels: &[u8], tw: usize, th: usize, u: f32, v: f32, sky: &SkyView, time: f32) -> u8 {
    // `SKYSIZE` (128) -> 6*(SKYSIZE/2 - 1) = 6*63 = 378, the C dome scale.
    const SKY_DOME_SCALE: f32 = 6.0 * (128.0 / 2.0 - 1.0);
    const SKY_SPEED: f32 = 8.0;

    // Screen offsets, scaled exactly as D_Sky_uv_To_st (8192/longest), but we work
    // in our projection: a pixel `(u,v)` corresponds to camera-space direction
    // proportional to `right*(u-cx)/focal + up*-(v-cy)/focal + forward`. Scaling
    // by 4096 forward (the C uses `4096*vpn` with `8192*offset`) keeps the same
    // ratio; the subsequent normalise removes the absolute scale.
    let longest = if sky.longest > 0.0 { sky.longest } else { 1.0 };
    let wu = 8192.0 * (u - sky.cx) / longest;
    let wv = 8192.0 * (sky.cy - v) / longest;

    let mut end = [
        4096.0 * sky.forward[0] + wu * sky.right[0] + wv * sky.up[0],
        4096.0 * sky.forward[1] + wu * sky.right[1] + wv * sky.up[1],
        4096.0 * sky.forward[2] + wu * sky.right[2] + wv * sky.up[2],
    ];
    end[2] *= 3.0; // vertical squash so the dome is shallow
    let (dir, len) = normalize(end);
    if len == 0.0 {
        return 0;
    }

    let scroll = time * SKY_SPEED;
    // s/t in texels: the dome scale projects the direction onto the layer. We feed
    // these to `sky_texel` with time=0 (the scroll is folded into s/t here), but
    // `sky_texel` adds its own per-layer scroll — so pass the raw projected coords
    // and let the two-layer overlay/background lookup add the front/back drift.
    let s = scroll + SKY_DOME_SCALE * dir[0];
    let t = scroll + SKY_DOME_SCALE * dir[1];
    // `sky_texel` expects (s,t) that it scales by 0.125; pre-multiply by 8 so the
    // dome projection lands at a sensible cloud scale after its internal *0.125.
    sky_texel(pixels, tw, th, s * 8.0, t * 8.0, 0.0)
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
    /// Sky: project the per-pixel VIEW DIRECTION onto the scrolling sky dome
    /// (`D_Sky_uv_To_st`) rather than mapping wall `(s,t)`. Fullbright.
    Sky { time: f32, view: SkyView },
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

/// `R_TextureAnimation` (`r_surf.c`): pick the texture index to draw for an
/// animated (`+`-prefixed) base texture at game `time`, given the drawing
/// entity's `frame` field.
///
/// Returns the index into `bsp.textures` of the frame to render:
///  * If `ent_frame != 0` and the base has an `alternate_anims` cycle, switch to
///    that cycle's first frame as the new base (the `+a..+j` set).
///  * If the (possibly switched) base is not animated (`anim == None`), return it
///    unchanged.
///  * Otherwise walk `anim_next` from the base until the frame whose
///    `[anim_min, anim_max)` window contains `relative = (int)(time*10) %
///    anim_total`, exactly like the C `while (anim_min > rel || anim_max <= rel)`.
///
/// SAFETY: the walk is bounded (the C bounds it at 100 hops; we bound it at the
/// texture count) and every lookup is `.get()`-checked, so a broken cycle in
/// malformed map data returns the last reachable frame instead of looping or
/// panicking (the C `Sys_Error`s).
fn texture_animation(bsp: &Bsp, base_index: usize, ent_frame: i32, time: f32) -> usize {
    // Resolve the entity-frame alternate switch first (C: `if (currententity->
    // frame) { if (base->alternate_anims) base = base->alternate_anims; }`).
    let mut idx = base_index;
    if ent_frame != 0 {
        if let Some(Some(tx)) = bsp.textures.get(idx) {
            if let Some(anim) = tx.anim {
                if let Some(alt) = anim.alternate {
                    idx = alt;
                }
            }
        }
    }

    // If not animated, return as-is.
    let anim = match bsp.textures.get(idx) {
        Some(Some(tx)) => match tx.anim {
            Some(a) if a.total > 0 => a,
            _ => return idx,
        },
        _ => return idx,
    };

    // relative = (int)(cl.time*10) % anim_total, in tenths of a second.
    let rel = ((time * 10.0) as i64).rem_euclid(anim.total as i64) as i32;

    // Walk anim_next until rel is inside [anim_min, anim_max).
    let mut cur = idx;
    let mut cur_anim = anim;
    let limit = bsp.textures.len().max(1) + 1;
    let mut count = 0usize;
    while cur_anim.min > rel || cur_anim.max <= rel {
        count += 1;
        if count > limit {
            break; // broken cycle: stop rather than loop (C: Sys_Error).
        }
        cur = cur_anim.next;
        match bsp.textures.get(cur) {
            Some(Some(tx)) => match tx.anim {
                Some(a) => cur_anim = a,
                None => break,
            },
            _ => break,
        }
    }
    cur
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
    colormap: Option<&[u8]>,
) {
    let w = image.w;
    let h = image.h;
    if w == 0 || h == 0 || tw == 0 || th == 0 || pixels.len() < tw * th {
        return;
    }
    // Only use a correctly-sized colormap; a malformed one falls back to the
    // linear multiply (never reads out of bounds).
    let colormap = colormap.filter(|cm| cm.len() >= COLORMAP_LEN);
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
    // The barycentric weights are LINEAR in the pixel position, so step them
    // incrementally (3 adds/pixel) instead of three full `edge()` cross-products
    // per pixel — the classic span-rasteriser speedup Quake's D_DrawSpans used.
    // `w_i` is recomputed exactly via `edge()` at each ROW START (so drift never
    // accumulates across rows), then advanced by `dw_i_dx` across the row. The
    // accumulation differs from a per-pixel recompute by at most a few ULPs over a
    // row, which can only flip the inside-test on a sub-pixel sliver at a triangle
    // edge — visually identical.
    let dw0dx = -(v2.y - v1.y) * inv_area;
    let dw1dx = -(v0.y - v2.y) * inv_area;
    let dw2dx = -(v1.y - v0.y) * inv_area;

    for py in min_y..=max_y {
        let sy = py as f32 + 0.5;
        let sx0 = min_x as f32 + 0.5;
        let mut w0 = edge(v1.x, v1.y, v2.x, v2.y, sx0, sy) * inv_area;
        let mut w1 = edge(v2.x, v2.y, v0.x, v0.y, sx0, sy) * inv_area;
        let mut w2 = edge(v0.x, v0.y, v1.x, v1.y, sx0, sy) * inv_area;
        for px in min_x..=max_x {
            // A labelled block so the early-outs can `break 'pixel` to the per-pixel
            // weight step below (a `continue` would skip the increment and desync).
            'pixel: {
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                break 'pixel;
            }
            // Perspective-correct depth (1/z interpolation, then invert), matching
            // Quake's `zi`-keyed z-buffer and the flat path. `inv_z` is reused for
            // the s/t perspective divide below, so this costs nothing extra.
            let inv_z = w0 * iz0 + w1 * iz1 + w2 * iz2;
            if inv_z <= 0.0 {
                break 'pixel;
            }
            let depth = 1.0 / inv_z;
            let idx = (py as usize) * w + (px as usize);
            let zc = match zbuf.get_mut(idx) {
                Some(z) => z,
                None => break 'pixel,
            };
            if depth >= *zc {
                break 'pixel;
            }
            let sx = px as f32 + 0.5;
            // Perspective divide reuses `depth` (= 1/inv_z) as a multiply instead of
            // two more reciprocals — the affine numerators times 1/z. (Differs from
            // `/inv_z` by at most a ULP, which never crosses a texel boundary.)
            let s = (w0 * soz0 + w1 * soz1 + w2 * soz2) * depth;
            let t = (w0 * toz0 + w1 * toz1 + w2 * toz2) * depth;

            // Resolve the palette index and per-pixel brightness per surface
            // mode. Liquids/sky are fullbright (brightness 1.0, no lightmap);
            // walls keep the lightmap-or-`shade` brightness.
            let (texel, brightness) = match mode {
                SurfaceMode::Normal => {
                    let tx = (s as i64).rem_euclid(tw as i64) as usize;
                    let ty = (t as i64).rem_euclid(th as i64) as usize;
                    let p = match pixels.get(ty * tw + tx) {
                        Some(&p) => p as usize,
                        None => break 'pixel,
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
                        None => break 'pixel,
                    };
                    (p, 1.0)
                }
                SurfaceMode::Sky { time, view } => {
                    // Project the per-pixel VIEW DIRECTION onto the scrolling sky
                    // dome (`D_Sky_uv_To_st`) — the sky no longer uses wall (s,t).
                    // `sx`/`sy` are the pixel centre in screen space; fullbright.
                    (sky_texel_view(pixels, tw, th, sx, sy, &view, time) as usize, 1.0)
                }
            };
            *zc = depth;
            if let Some(p) = image.rgb.get_mut(idx) {
                match colormap {
                    // Quake's exact software shading: pick a colormap ROW from
                    // the brightness, then index the colormap to get a PALETTE
                    // INDEX, which is finally looked up in the palette. This is
                    // an INDEX lookup (no RGB multiply) and so can never
                    // overbright past the base colour. Liquids/sky are
                    // fullbright (brightness 1.0) but route through the brightest
                    // row 0 (`colormap[texel]`) — `colormap_row(1.0)` is *not*
                    // row 0, so fullbright surfaces force the row explicitly.
                    Some(cm) => {
                        let row = match mode {
                            SurfaceMode::Normal => colormap_row(brightness),
                            // Turb/Sky are fullbright: the brightest row.
                            SurfaceMode::Turb { .. } | SurfaceMode::Sky { .. } => 0,
                        };
                        // row < COLORMAP_ROWS and texel < 256, so this index is
                        // < COLORMAP_LEN <= cm.len() (checked above).
                        let pal_index = cm[row * 256 + texel] as usize;
                        *p = palette[pal_index];
                    }
                    // Fallback: the original linear `palette[texel] * brightness`
                    // (byte-for-byte unchanged when no colormap is supplied).
                    None => {
                        let rgb = palette[texel];
                        *p = [
                            (rgb[0] as f32 * brightness).clamp(0.0, 255.0) as u8,
                            (rgb[1] as f32 * brightness).clamp(0.0, 255.0) as u8,
                            (rgb[2] as f32 * brightness).clamp(0.0, 255.0) as u8,
                        ];
                    }
                }
            }
            } // 'pixel
            // Step the barycentric weights one pixel across the row (always, even on
            // an early-out, so the running values stay in sync with `px`).
            w0 += dw0dx;
            w1 += dw1dx;
            w2 += dw2dx;
        }
    }
}

/// Fast rasteriser for a wall whose lit+colormapped surface block is already baked
/// (see [`face_surf_block`]) — Quake's `D_DrawSpans` over a cached surface. Same
/// perspective-correct projection, incremental-edge stepping, near-clip handling
/// and z-test as [`raster_triangle_tex`], but the inner pixel is ONE block read
/// (the texture, lightmap and colormap are already folded into the block) plus a
/// palette lookup, instead of a texture sample + bilinear lightmap + colormap row
/// + colormap index per pixel. This is the warm-frame hot path for static walls.
#[allow(clippy::too_many_arguments)]
fn raster_triangle_cached(
    image: &mut Image,
    zbuf: &mut [f32],
    v0: ProjT,
    v1: ProjT,
    v2: ProjT,
    block: &[u8],
    bw: usize,
    bh: usize,
    texmins: [f32; 2],
    palette: &[[u8; 3]; 256],
) {
    let w = image.w;
    let h = image.h;
    if w == 0 || h == 0 || bw == 0 || bh == 0 || block.len() < bw.saturating_mul(bh) {
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
    let dw0dx = -(v2.y - v1.y) * inv_area;
    let dw1dx = -(v0.y - v2.y) * inv_area;
    let dw2dx = -(v1.y - v0.y) * inv_area;
    // Per-pixel-x derivatives of the perspective accumulators. `inv_z`, `s/z` and
    // `t/z` are each EXACTLY linear in screen x (they are linear combinations of the
    // barycentric weights, which themselves step by `dw*dx` per pixel), so they can
    // be advanced with a single add per pixel instead of re-dotting the three weights
    // every pixel — removing ~9 multiplies/pixel. We still step w0/w1/w2 for the
    // edge inside-test. (Additive accumulation differs from the per-pixel re-dot by
    // a few ULPs across a span — the same negligible drift class as the incremental
    // edge stepping; verified to leave the world render essentially unchanged.)
    let dinvz = iz0 * dw0dx + iz1 * dw1dx + iz2 * dw2dx;
    let dsoz = soz0 * dw0dx + soz1 * dw1dx + soz2 * dw2dx;
    let dtoz = toz0 * dw0dx + toz1 * dw1dx + toz2 * dw2dx;
    let (bw_i, bh_i) = (bw as i64, bh as i64);

    // Local written-pixel tally (overdraw metric), folded into the profiler ONCE at
    // the end so the hot loop never touches a thread-local.
    let mut drawn = 0u64;
    // min_x/max_x are clamped to 0..w and min_y/max_y to 0..h above, so every row's
    // [xa, xb] slice of the framebuffer and z-buffer is provably in bounds. Taking a
    // per-row &mut slice and indexing it with the LOCAL offset `px - xa` lets the
    // compiler drop the per-pixel bounds checks the old `get_mut(idx)` paid on every
    // covered pixel — the span-oriented access Quake's D_DrawSpans used. The texel
    // read still clamps (block extent is independent of the screen rect). Output is
    // identical: same pixels, same values, same z-writes.
    let xa = min_x as usize;
    let xb = max_x as usize;
    let span = xb - xa + 1;
    for py in min_y..=max_y {
        let sy = py as f32 + 0.5;
        let sx0 = xa as f32 + 0.5;
        let mut w0 = edge(v1.x, v1.y, v2.x, v2.y, sx0, sy) * inv_area;
        let mut w1 = edge(v2.x, v2.y, v0.x, v0.y, sx0, sy) * inv_area;
        let mut w2 = edge(v0.x, v0.y, v1.x, v1.y, sx0, sy) * inv_area;
        // Row-start perspective accumulators (exact dot at the first pixel of the
        // row; stepped by the derivatives after each pixel).
        let mut inv_z = w0 * iz0 + w1 * iz1 + w2 * iz2;
        let mut soz = w0 * soz0 + w1 * soz1 + w2 * soz2;
        let mut toz = w0 * toz0 + w1 * toz1 + w2 * toz2;
        let row = (py as usize) * w;
        let zrow = &mut zbuf[row + xa..row + xa + span];
        let crow = &mut image.rgb[row + xa..row + xa + span];
        for k in 0..span {
            'pixel: {
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    break 'pixel;
                }
                if inv_z <= 0.0 {
                    break 'pixel;
                }
                let depth = 1.0 / inv_z;
                let zc = &mut zrow[k];
                if depth >= *zc {
                    break 'pixel;
                }
                let s = soz * depth;
                let t = toz * depth;
                // Nearest surface texel within the block extent (the block is 1:1
                // with surface texels at mip 0).
                let bx = ((s - texmins[0]) as i64).clamp(0, bw_i - 1) as usize;
                let by = ((t - texmins[1]) as i64).clamp(0, bh_i - 1) as usize;
                let pal_idx = block[by * bw + bx] as usize;
                *zc = depth;
                crow[k] = palette[pal_idx];
                drawn += 1;
            }
            w0 += dw0dx;
            w1 += dw1dx;
            w2 += dw2dx;
            inv_z += dinvz;
            soz += dsoz;
            toz += dtoz;
        }
    }
    stat(|s| s.world_pixels += drawn);
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
    draw_world_textured(&mut image, &mut zbuf, bsp, cam, palette, &turb, 0.0, &NEUTRAL_LIGHTSTYLE_SCALES, &[], None);
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
/// classified against the node's plane: `dot(normal, p) - dist > 0` takes
/// `children[0]` (front), otherwise `children[1]` (back; on-plane goes back, per
/// C `Mod_PointInLeaf`). A *negative* child
/// encodes a leaf as `-(child) - 1`; a non-negative child is the next node.
///
/// Returns the leaf index, or `None` if the model/headnode/plane/child indices
/// are malformed or out of range (every access is bounds-checked, so this never
/// panics on corrupt data). A bounded iteration guard prevents a cyclic/corrupt
/// node graph from looping forever.
///
/// `pub` because the sound front-end also needs the VIEW leaf each frame: the
/// four automatic ambient channels read `leaf.ambient_level[]` at the listener
/// position (`S_UpdateAmbientSounds` -> `Mod_PointInLeaf`); see [`crate::snd`].
pub fn point_in_leaf(bsp: &Bsp, p: Vec3) -> Option<usize> {
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
        // front (child[0]) only when strictly in front; the exactly-on-plane case
        // (d == 0) goes to the back child, matching C `Mod_PointInLeaf` (`if (d > 0)`)
        // and the sibling recursive_light_point descent.
        let child = if d > 0.0 {
            *node.children.first()?
        } else {
            *node.children.get(1)?
        };
        node_index = child as i32;
    }

    // Exceeded the step guard: treat as malformed.
    None
}

/// `R_LightPoint` (`r_light.c`): sample the baked world light at world-space
/// point `p`, returning a 0..255 brightness (the average of the active light
/// styles at the surface directly below `p`).
///
/// Casts a ray straight down (`p` -> `p - 2048z`) through the worldmodel's BSP
/// and, at the first lightmapped surface the segment crosses, reads that
/// surface's lightmap luxel (summing each active style's value scaled by
/// `light_styles`, like `RecursiveLightPoint`). Returns:
///  * the sampled brightness (`0..=255`) when a lightmapped surface is hit;
///  * `0` when the ray hits a tiled (sky/liquid) surface or a surface with no
///    samples (the C returns 0 there);
///  * `255` (fullbright) when the map has no lighting data at all
///    (`!cl.worldmodel->lightdata`), matching the C early-out.
///
/// `light_styles` scales each style's luxel (1.0 == the C's `d_lightstylevalue`
/// of 256 mapped to neutral). SAFETY: the recursion is depth-bounded and every
/// index is `.get()`-checked, so corrupt node/plane/face data yields a default
/// (no light) rather than a panic or unbounded recursion.
fn r_light_point(bsp: &Bsp, p: Vec3, light_styles: &[f32; LIGHTSTYLES]) -> f32 {
    if bsp.lighting.is_empty() {
        return 255.0; // C: `if (!worldmodel->lightdata) return 255;`
    }
    let headnode = match bsp.models.first().and_then(|m| m.headnode.first().copied()) {
        Some(h) => h,
        None => return 0.0,
    };
    let end = [p[0], p[1], p[2] - 2048.0];
    let depth = bsp.nodes.len().saturating_add(2);
    match recursive_light_point(bsp, headnode, p, end, light_styles, depth) {
        Some(r) => r.max(0.0),
        None => 0.0, // C: `if (r == -1) r = 0;`
    }
}

/// One step of `RecursiveLightPoint`. `node` is a child reference (negative =>
/// leaf, "didn't hit anything"). Returns `Some(brightness)` on a hit,
/// `None` for "didn't hit anything" (the C `-1`). `depth` bounds the recursion.
fn recursive_light_point(
    bsp: &Bsp,
    node: i32,
    start: Vec3,
    end: Vec3,
    light_styles: &[f32; LIGHTSTYLES],
    depth: usize,
) -> Option<f32> {
    if depth == 0 {
        return None;
    }
    if node < 0 {
        return None; // leaf: didn't hit anything (C `node->contents < 0`).
    }
    let ni: usize = node.try_into().ok()?;
    let node_rec = bsp.nodes.get(ni)?;
    let pi: usize = (node_rec.planenum as i64).try_into().ok()?;
    let plane = bsp.planes.get(pi)?;

    let front = dot(start, plane.normal) - plane.dist;
    let back = dot(end, plane.normal) - plane.dist;
    let side = front < 0.0; // C `side = front < 0`
    let side_child = |s: bool| -> Option<i32> {
        if s {
            node_rec.children.get(1).copied().map(|c| c as i32)
        } else {
            node_rec.children.first().copied().map(|c| c as i32)
        }
    };

    // If both endpoints are on the same side, recurse into that side only.
    if (back < 0.0) == side {
        return recursive_light_point(bsp, side_child(side)?, start, end, light_styles, depth - 1);
    }

    // Split: compute the midpoint on the plane.
    let denom = front - back;
    if denom == 0.0 {
        return recursive_light_point(bsp, side_child(side)?, start, end, light_styles, depth - 1);
    }
    let frac = front / denom;
    let mid = [
        start[0] + (end[0] - start[0]) * frac,
        start[1] + (end[1] - start[1]) * frac,
        start[2] + (end[2] - start[2]) * frac,
    ];

    // Go down the front side first.
    if let Some(r) = recursive_light_point(bsp, side_child(side)?, start, mid, light_styles, depth - 1)
    {
        return Some(r); // hit something
    }
    // (back<0)==side already handled above; here the sides differ, so check this
    // node's surfaces for an impact, then go down the back side.
    if let Some(r) = light_point_check_node(bsp, node_rec, mid, light_styles) {
        return Some(r);
    }
    recursive_light_point(bsp, side_child(!side)?, mid, end, light_styles, depth - 1)
}

/// Check this node's surfaces for the impact point `mid`, porting the surface
/// loop in `RecursiveLightPoint`. Returns `Some(brightness)` if `mid` lands on a
/// lightmapped surface (including `Some(0.0)` for a surface with no samples),
/// else `None` (the segment did not land on any of this node's surfaces).
fn light_point_check_node(
    bsp: &Bsp,
    node: &crate::bsp::DNode,
    mid: Vec3,
    light_styles: &[f32; LIGHTSTYLES],
) -> Option<f32> {
    use crate::bsp::TEX_SPECIAL;
    let first = node.firstface as usize;
    let count = node.numfaces as usize;
    let mut world_poly: Vec<Vec3> = Vec::new();
    for face_index in first..first.saturating_add(count) {
        let face = match bsp.faces.get(face_index) {
            Some(f) => f,
            None => continue,
        };
        let ti = match (face.texinfo as i64)
            .try_into()
            .ok()
            .and_then(|i: usize| bsp.texinfo.get(i))
        {
            Some(t) => t,
            None => continue,
        };
        // Tiled surfaces (sky/liquid) have no lightmaps (C `SURF_DRAWTILED`).
        if ti.flags & TEX_SPECIAL != 0 {
            continue;
        }

        // Surface coordinate of `mid`.
        let s = mid[0] * ti.vecs[0][0] + mid[1] * ti.vecs[0][1] + mid[2] * ti.vecs[0][2] + ti.vecs[0][3];
        let t = mid[0] * ti.vecs[1][0] + mid[1] * ti.vecs[1][1] + mid[2] * ti.vecs[1][2] + ti.vecs[1][3];

        // Need the surface extents (texmins/extent) to test the bounds, exactly
        // as `RecursiveLightPoint` uses surf->texturemins / surf->extents.
        if !face_world_poly(bsp, face, &mut world_poly) {
            continue;
        }
        let (texmins, extent) = match surface_extents(ti, &world_poly) {
            Some(v) => v,
            None => continue,
        };
        // C RecursiveLightPoint declares s,t,ds,dt as int: the surface coordinate is
        // TRUNCATED to int before the texmins subtract and the >>4 luxel select. Do
        // the same integer arithmetic so the chosen luxel matches the C exactly
        // (the float path could drift one luxel near integer boundaries).
        let s_i = s as i32; // (int) truncation toward zero, as the C cast
        let t_i = t as i32;
        let ds = s_i - texmins[0];
        let dt = t_i - texmins[1];
        if ds < 0 || dt < 0 || ds > extent[0] || dt > extent[1] {
            continue;
        }

        // The point is on this surface. With no samples the C returns 0.
        if face.lightofs < 0 {
            return Some(0.0);
        }
        let lmw = (extent[0] / 16 + 1) as usize;
        let lmh = (extent[1] / 16 + 1) as usize;
        let block = match lmw.checked_mul(lmh) {
            Some(b) => b,
            None => return Some(0.0),
        };
        let start: usize = match face.lightofs.try_into() {
            Ok(s) => s,
            Err(_) => return Some(0.0),
        };
        // Luxel coordinate within the block (C `ds>>4`, `dt>>4`; ds,dt >= 0 here).
        let lx = ((ds >> 4) as i64).clamp(0, lmw as i64 - 1) as usize;
        let ly = ((dt >> 4) as i64).clamp(0, lmh as i64 - 1) as usize;
        let luxel = ly * lmw + lx;

        // Sum each active style's luxel, scaled by its light-style value. The C
        // multiplies by `d_lightstylevalue` (256 == neutral) then `>>8`; here the
        // neutral scale is 1.0, so we sum `luxel_byte * scale`.
        let mut r = 0.0f32;
        for (k, &style) in face.styles.iter().enumerate() {
            if style == STYLE_NONE {
                break;
            }
            let off = match start.checked_add(k * block).and_then(|o| o.checked_add(luxel)) {
                Some(o) => o,
                None => break,
            };
            let sample = match bsp.lighting.get(off) {
                Some(&b) => b as f32,
                None => break,
            };
            let scale = light_styles.get(style as usize).copied().unwrap_or(1.0);
            r += sample * scale;
        }
        return Some(r);
    }
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

/// A brush face vertex in *view space* (`vx`/`vy`/`vz` along the camera's
/// right/up/forward axes) carrying the per-vertex texture coordinates `(s, t)`.
///
/// All five fields are *affine* functions of the world-space position, so along
/// a straight polygon edge they interpolate linearly with the *same* parameter.
/// That is what makes near-plane clipping a plain componentwise lerp: the
/// clipped vertex's `(vx, vy, vz, s, t)` is the lerp of the edge endpoints, and
/// projecting it afterwards yields the perspective-correct screen point.
#[derive(Clone, Copy)]
struct VView {
    vx: f32,
    vy: f32,
    vz: f32,
    s: f32,
    t: f32,
}

/// The near-clip plane, `vz == NEAR`; a vertex is *inside* iff `vz > NEAR`. Must
/// match the `NEAR` used by the draw passes (1.0).
const NEAR_PLANE: f32 = 1.0;

/// Componentwise lerp of two view-space vertices by `alpha` in `[0, 1]`
/// (`a` at 0, `b` at 1). Because every field is affine in world position, this
/// is the exact value of the attribute at the lerped world point.
fn vview_lerp(a: &VView, b: &VView, alpha: f32) -> VView {
    VView {
        vx: a.vx + (b.vx - a.vx) * alpha,
        vy: a.vy + (b.vy - a.vy) * alpha,
        vz: a.vz + (b.vz - a.vz) * alpha,
        s: a.s + (b.s - a.s) * alpha,
        t: a.t + (b.t - a.t) * alpha,
    }
}

/// Sutherland–Hodgman clip of a single convex/planar polygon (given in view
/// space) against the one near plane `vz >= NEAR_PLANE`.
///
/// A vertex is *inside* iff `vz > NEAR_PLANE`. Walking each edge `(cur, next)`
/// (with `next` wrapping to the first vertex), the output keeps `cur` when it is
/// inside and emits the near-plane crossing vertex whenever `cur` and `next` lie
/// on opposite sides of `vz == NEAR_PLANE`. The crossing parameter for an edge
/// `A -> B` is `alpha = (NEAR_PLANE - A.vz) / (B.vz - A.vz)`, and the new vertex
/// is the [`vview_lerp`] of `A`/`B` by `alpha` — so its `vz` becomes exactly
/// `NEAR_PLANE` and its `(vx, vy, s, t)` are the matching linear interpolations.
///
/// Behaviour at the extremes (important for *no* regression on the common case):
///  * A polygon **fully inside** (every `vz > NEAR_PLANE`) is returned with its
///    vertices **unchanged and in the same order** — no crossing is ever emitted,
///    so the result is byte-identical to the unclipped input.
///  * A polygon **fully behind** (every `vz <= NEAR_PLANE`) yields no inside
///    vertices and no crossings, so an empty (`< 3`) result is returned and the
///    caller skips the face.
///
/// Convenience wrapper: clip against the near plane into a fresh `Vec` (for cold
/// paths and tests). The per-face hot paths call [`clip_poly_near_into`] with a
/// reused scratch buffer instead.
fn clip_poly_near(input: &[VView]) -> Vec<VView> {
    let mut out = Vec::new();
    clip_poly_near_into(input, &mut out);
    out
}

/// Clip `input` against the near plane, writing the result into `out` (cleared
/// first). `out` is a caller-owned scratch buffer reused across faces so the
/// overwhelmingly common per-face call allocates nothing. The vertices written
/// are byte-identical to the previous return-a-fresh-`Vec` version.
fn clip_poly_near_into(input: &[VView], out: &mut Vec<VView>) {
    out.clear();
    let n = input.len();
    if n == 0 {
        return;
    }
    // Fast path: a polygon entirely in front of the near plane is copied
    // unchanged (same vertices, same order). This keeps the overwhelmingly
    // common case a verbatim copy, guaranteeing no rasteriser regression.
    if input.iter().all(|v| v.vz > NEAR_PLANE) {
        out.extend_from_slice(input);
        return;
    }
    out.reserve(n + 1);
    for i in 0..n {
        let cur = &input[i];
        let next = &input[(i + 1) % n];
        let cur_in = cur.vz > NEAR_PLANE;
        let next_in = next.vz > NEAR_PLANE;
        if cur_in {
            out.push(*cur);
        }
        // Emit a crossing vertex whenever the edge straddles the plane. The
        // denominator is non-zero precisely because the endpoints differ in
        // inside-ness, hence differ in `vz`.
        if cur_in != next_in {
            let denom = next.vz - cur.vz;
            if denom != 0.0 {
                let alpha = (NEAR_PLANE - cur.vz) / denom;
                out.push(vview_lerp(cur, next, alpha));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// View-frustum culling (Quake's R_CullBox) + per-face static caches
// ---------------------------------------------------------------------------
//
// Big maps (e1m3) spend most of their time in the per-face world loop:
// reconstructing each visible face's polygon, projecting it, and *rebuilding*
// its combined lightmap every frame. Two caches below cut that without changing
// a single output pixel:
//
//  * A **view frustum** (4 side planes + near) derived from the camera, used to
//    reject — *before* any polygon/projection/raster work — faces whose static
//    world-space AABB lies entirely outside the view. This is purely a SKIP
//    decision placed ahead of the existing PVS/backface/near-clip pipeline; a
//    culled face contributes zero drawn pixels, so the image is unchanged.
//  * A **per-face static-geometry cache** (world polygon, normal, centroid,
//    surface extents, AABB) computed once for the world model, plus a
//    **lightmap surface cache** (Quake's `R_BuildLightMap` cache) that reuses a
//    face's combined luxel buffer while its resolved style scales are unchanged.

/// A view frustum: four side planes (left/right/bottom/top) plus the near
/// plane, all with inward-pointing normals. A point `p` is inside the frustum
/// iff `dot(plane.normal, p) >= plane.dist` for every plane. A box is culled iff
/// it lies entirely on the *outside* (`< dist`) of some plane — i.e.
/// `box_on_plane_side(...) == 2` (the same predicate as Quake's `R_CullBox`).
struct Frustum {
    planes: [crate::math::Plane; 5],
}

impl Frustum {
    /// Derive the frustum from the camera and the framebuffer aspect.
    ///
    /// The side planes are built to EXACTLY match the screen rectangle the
    /// rasteriser draws into. The rasteriser projects a view-space vertex
    /// `(vx, vy, vz)` (along `right`/`up`/`forward`) to
    /// `x = cx + focal*vx/vz`, `y = cy - focal*vy/vz` with `cx = w/2`,
    /// `cy = h/2`, `focal = cx / tan(fov/2)`. On-screen means `0 <= x < w` and
    /// `0 <= y < h`, i.e. `|vx/vz| <= cx/focal = tx` and `|vy/vz| <= cy/focal =
    /// ty`. So the horizontal half-extent is `tx = tan(fov/2)` and the vertical
    /// is `ty = (cy/cx)*tx = (h/w)*tan(fov/2)`. The inward side-plane normals in
    /// VIEW coordinates are therefore:
    ///   left   `( 1, 0, tx)`  (inside: `vx + tx*vz >= 0`)
    ///   right  `(-1, 0, tx)`
    ///   bottom `( 0, 1, ty)`
    ///   top    `( 0,-1, ty)`
    /// each transformed to world space via `n = nx*right + ny*up + nz*forward`.
    /// Every plane passes through the camera origin, so `dist = dot(pos, n)`.
    /// The near plane has normal `forward`, `dist = dot(pos + forward*NEAR,
    /// forward)`, matching `clip_poly_near`'s `vz >= NEAR_PLANE` test.
    ///
    /// CONSERVATIVENESS: these planes bound precisely the angular region the
    /// rasteriser can draw to (the screen rectangle), so a face that produces
    /// any on-screen pixel has at least one vertex inside all five planes — its
    /// AABB then straddles or is inside every plane and is never culled. The
    /// normals are NOT normalised: the side/cull test only uses the SIGN of
    /// `dot(n, corner) - dist`, which a positive scale leaves unchanged, so
    /// skipping the normalise costs nothing and avoids a sqrt rounding step.
    fn from_camera(cam: &Camera, w: usize, h: usize) -> Frustum {
        let (forward, right, up) = cam.basis();
        let half_fov = (cam.fov_deg as f64 * 0.5).to_radians();
        let tan_half = half_fov.tan();
        let cxf = w as f32 / 2.0;
        let cyf = h as f32 / 2.0;
        // tx matches the rasteriser's cx/focal == tan(fov/2); guard the
        // degenerate focal (tan ~ 0) the draw path falls back on (focal = cx,
        // i.e. tx = 1.0) so the frustum stays consistent with what is drawn.
        let tx = if tan_half.abs() < 1e-6 { 1.0f32 } else { tan_half as f32 };
        // ty = (cy/cx)*tx; with cx==0 (zero-width) fall back to tx (the loop
        // never runs for w==0 anyway).
        let ty = if cxf != 0.0 { (cyf / cxf) * tx } else { tx };

        // View-space inward normals (see doc comment), in (right, up, forward)
        // components.
        let view_normals: [[f32; 3]; 4] = [
            [1.0, 0.0, tx],  // left
            [-1.0, 0.0, tx], // right
            [0.0, 1.0, ty],  // bottom
            [0.0, -1.0, ty], // top
        ];

        let to_world = |n: [f32; 3]| -> Vec3 {
            [
                n[0] * right[0] + n[1] * up[0] + n[2] * forward[0],
                n[0] * right[1] + n[1] * up[1] + n[2] * forward[1],
                n[0] * right[2] + n[1] * up[2] + n[2] * forward[2],
            ]
        };

        // mplane_t-style planes (carry signbits so box_on_plane_side picks the
        // right corners). Each side plane passes through cam.pos.
        let mut planes: [crate::math::Plane; 5] = [
            crate::math::Plane::new([1.0, 0.0, 0.0], 0.0),
            crate::math::Plane::new([1.0, 0.0, 0.0], 0.0),
            crate::math::Plane::new([1.0, 0.0, 0.0], 0.0),
            crate::math::Plane::new([1.0, 0.0, 0.0], 0.0),
            crate::math::Plane::new([1.0, 0.0, 0.0], 0.0),
        ];
        for (i, vn) in view_normals.iter().enumerate() {
            let nw = to_world(*vn);
            planes[i] = crate::math::Plane::new(nw, dot(cam.pos, nw));
        }
        // Near plane: inward normal = forward, through `pos + forward*NEAR`.
        let near_dist = dot(cam.pos, forward) + NEAR_PLANE;
        planes[4] = crate::math::Plane::new(forward, near_dist);

        Frustum { planes }
    }

    /// `R_CullBox`: true (cull) iff the AABB `[mins, maxs]` is entirely on the
    /// outside of some frustum plane (`box_on_plane_side == 2`). Conservative:
    /// a box that straddles or is inside every plane is kept.
    #[inline]
    fn culls(&self, mins: Vec3, maxs: Vec3) -> bool {
        for p in &self.planes {
            if crate::math::box_on_plane_side(mins, maxs, p) == 2 {
                return true;
            }
        }
        false
    }
}

/// Per-face STATIC geometry for the world model, computed once and reused every
/// frame (the world model never moves, so its faces' polygons, normals,
/// centroids, surface extents and AABBs are frame-invariant). Caching this skips
/// the surfedge/edge/vertex walk, the centroid loop, and `surface_extents` for
/// every visible face on every frame.
#[derive(Clone)]
struct FaceGeom {
    /// Reconstructed world-space polygon (same vertices/order `face_world_poly`
    /// produces — so downstream projection/texturing is byte-identical). Behind an
    /// `Rc` so the per-frame cache fetch (`face_geom_cached`, twice per face) is an
    /// O(1) refcount bump, not a deep `Vec` copy — the vertices are immutable once
    /// built.
    poly: std::rc::Rc<Vec<Vec3>>,
    /// Outward face normal (`face_normal`), or `None` if the plane was bad.
    normal: Option<Vec3>,
    /// Polygon centroid (the exact same accumulate-then-`*1/n` the loop used).
    center: Vec3,
    /// World-space AABB of `poly` (for the frustum cull).
    mins: Vec3,
    maxs: Vec3,
    /// `true` when `face_world_poly` failed (degenerate/out-of-range face); the
    /// loop then skips it exactly as before.
    bad: bool,
}

/// The world-model static-geometry cache. Keyed by a cheap world fingerprint so
/// it self-invalidates on a changelevel (face indices are meaningless after the
/// BSP is swapped). `geoms[i]` is lazily filled the first time face `i` is
/// reached; `Frustum` AABBs read from it.
struct GeomCache {
    fingerprint: WorldFingerprint,
    geoms: Vec<Option<FaceGeom>>,
}

/// A cheap identity for the loaded world. A changelevel always re-parses the BSP
/// (new `faces`/`lighting`/… lengths AND a fresh `&Bsp` address), so a mismatch
/// reliably means "different world -> the face-index-keyed caches are stale and
/// must be cleared". We combine the `&Bsp` pointer with several lump lengths so
/// the key changes if EITHER the address differs (the common case: a new map)
/// OR the lengths differ (guards against an allocator reusing a freed address
/// for a different-but-same-pointer Bsp). A face index is meaningless across a
/// world swap, so any mismatch forces a full cache rebuild.
#[derive(Clone, Copy, PartialEq, Eq)]
struct WorldFingerprint {
    ptr: usize,
    faces_len: usize,
    lighting_len: usize,
    planes_len: usize,
    vertexes_len: usize,
}

impl WorldFingerprint {
    fn of(bsp: &Bsp) -> WorldFingerprint {
        WorldFingerprint {
            ptr: bsp as *const Bsp as usize,
            faces_len: bsp.faces.len(),
            lighting_len: bsp.lighting.len(),
            planes_len: bsp.planes.len(),
            vertexes_len: bsp.vertexes.len(),
        }
    }
}

/// One cached face lightmap (Quake's `R_BuildLightMap` surface cache entry).
///
/// We cache ONLY the owned, combined-`f32` case (multi-style / non-neutral
/// scale, and NOT touched by a dynamic light): a single steady style-0 face at
/// neutral scale keeps borrowing the static bytes (no cache needed, already
/// byte-identical). The stored `luxels` are the exact buffer
/// `face_lightmap_dyn` produced, so reusing them is bit-for-bit identical to
/// rebuilding.
#[derive(Clone)]
struct LightCacheEntry {
    /// The resolved per-active-style SCALE values at build time (in stored slot
    /// order). The cache is valid only while these are bit-identical — torches
    /// animate at 10 Hz, so at 60 fps the scales are unchanged ~5/6 frames.
    style_scales: [f32; crate::bsp::MAXLIGHTMAPS],
    n_styles: usize,
    /// The cached combined luxel grid (no dynamic light folded in — see the
    /// "dlight_touched" gating in the wrapper).
    luxels: Vec<f32>,
    lmw: usize,
    lmh: usize,
    texmins: [f32; 2],
}

/// The lightmap surface cache, keyed by world fingerprint (so it clears on a
/// changelevel) + per-face style scales (so it rebuilds when a torch ticks).
struct LightCache {
    fingerprint: WorldFingerprint,
    entries: Vec<Option<LightCacheEntry>>,
}

/// One cached lit SURFACE block (Quake's `d_surf.c` surface cache entry, mip 0):
/// the face's texture with the lightmap shaded in AND resolved through the colormap
/// to a final palette index, one byte per surface texel. The rasteriser then reads
/// a single byte per screen pixel (then one palette lookup) instead of sampling the
/// texture, bilinear-interpolating the lightmap, and indexing the colormap per
/// pixel — moving all of that to a per-texel bake done ONCE and reused every frame.
#[derive(Clone)]
struct SurfCacheEntry {
    /// Active styles' resolved scale values at bake time (the cache key, same as the
    /// lightmap cache — a torch tick rebuilds the block).
    style_scales: [f32; crate::bsp::MAXLIGHTMAPS],
    n_styles: usize,
    /// Baked palette indices, `bw * bh`, row-major. `Rc` so a frame's draw clones
    /// the handle (a refcount bump), not the (possibly large) buffer.
    block: std::rc::Rc<Vec<u8>>,
    bw: usize,
    bh: usize,
    /// Surface-space origin of the block (`s = texmins[0] + i`, `t = texmins[1] + j`).
    texmins: [f32; 2],
}

/// The world model's lit-surface cache: its [`WorldFingerprint`] identity plus a
/// per-face slot (rebuilt when an animated style ticks). Held as a single
/// [`SURF_CACHE`] slot — only the world `Bsp` is ever cached (external brush
/// models bypass it), so it self-invalidates on a changelevel via the
/// fingerprint/`n_faces` check; see [`face_surf_block`].
struct SurfCache {
    fingerprint: WorldFingerprint,
    entries: Vec<Option<SurfCacheEntry>>,
}

thread_local! {
    /// Per-thread world static-geometry cache (one world at a time).
    static GEOM_CACHE: std::cell::RefCell<Option<GeomCache>> = const { std::cell::RefCell::new(None) };
    /// Per-thread lightmap surface cache.
    static LIGHT_CACHE: std::cell::RefCell<Option<LightCache>> = const { std::cell::RefCell::new(None) };
    /// Per-thread lit-surface (texel) cache for the world model (+ its inline
    /// submodels, which share the world `Bsp`). External brush models bypass it
    /// (they re-clone their `Bsp` every frame). See [`face_surf_block`].
    static SURF_CACHE: std::cell::RefCell<Option<SurfCache>> = const { std::cell::RefCell::new(None) };
    /// Granular render profiler (opt-in; see [`RenderStats`]). Off by default so the
    /// shared render path pays nothing in the live game / wasm.
    static RENDER_STATS: std::cell::RefCell<RenderStats> =
        const { std::cell::RefCell::new(RenderStats::ZERO) };
    static STATS_ON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Per-thread scratch for the per-face `R_MarkLights` dlight bit masks
    /// (`surf->dlightbits`). The world/submodel passes `mem::take` it for the
    /// duration of their face loop (they never nest) and put it back when done,
    /// so per-frame marking allocates nothing once the buffer has grown to the
    /// map's face count. See [`mark_dlights`].
    static DLIGHT_BITS_SCRATCH: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Granular per-phase render profiler — phase wall-times (ns) plus face/triangle/
/// pixel/cache counts for one [`render_scene_ext_sprited`] call. Populated only
/// while profiling is enabled via [`render_stats_begin`]; every counter site is
/// gated on the `STATS_ON` flag, so a normal (game/wasm) render touches none of it.
/// Use this to see WHERE a frame's time goes (which phase, overdraw, cache hit rate)
/// when tuning performance.
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderStats {
    /// Per-phase wall time in nanoseconds.
    pub world_ns: u64,
    pub submodel_ns: u64,
    pub external_ns: u64,
    pub alias_ns: u64,
    pub particle_ns: u64,
    pub sprite_ns: u64,
    pub viewmodel_ns: u64,
    /// World-model faces in the model-0 range.
    pub faces_total: u64,
    /// Faces skipped by the PVS visibility mask.
    pub faces_pvs_culled: u64,
    /// Faces skipped by the view-frustum AABB cull.
    pub faces_frustum_culled: u64,
    /// Faces that reached the rasteriser (world pass).
    pub faces_drawn: u64,
    /// Triangles submitted by the world pass.
    pub world_tris: u64,
    /// Pixels actually written by the world pass (overdraw proxy: a pixel covered by
    /// N drawn surfaces counts N times).
    pub world_pixels: u64,
    /// Lit-surface-cache hits / misses (world pass).
    pub surf_hits: u64,
    pub surf_misses: u64,
    /// Submodel pass: faces drawn, triangles, and pixels written. The submodel
    /// pass currently has NO surface cache and NO front-to-back ordering, so these
    /// reveal how much of the (often surprisingly large) submodel time is overdraw
    /// vs per-pixel lightmap+colormap cost — the next optimization target.
    pub sub_faces_visited: u64,
    pub sub_faces_drawn: u64,
    pub sub_surf_hits: u64,
    pub sub_surf_misses: u64,
    pub sub_tris: u64,
    /// Submodel lightmap rebuilds (every submodel face rebuilds via
    /// `face_lightmap_dyn` each frame — no cache).
    pub sub_lm_builds: u64,
    /// World-pass sub-phase timers (ns), for finding the FIXED per-face cost that
    /// dominates the frame independent of resolution. Only populated while
    /// profiling. `world_pvs_ns` is the once-per-frame PVS+frustum build; the rest
    /// accumulate across the per-face loop. `world_setup_ns` is the loop-body
    /// remainder (geom fetch + culls + projection + the per-pixel raster, since
    /// raster is not separately metered) and so also absorbs the `Instant` overhead
    /// of the nested light/surf timers — read it as "everything that isn't lightmap
    /// or surf-block lookup", not a precise figure.
    pub world_pvs_ns: u64,
    pub world_sort_ns: u64,
    pub world_setup_ns: u64,
    pub world_light_ns: u64,
    pub world_surf_ns: u64,
    /// Lit-surface-cache accounting inside `face_surf_block` (distinct from
    /// `surf_hits`, which only counts "returned a block"):
    ///  * `surf_cache_hits` — served from the world cache (the warm-frame norm).
    ///  * `surf_baked` — CACHED-path misses that re-baked a block. On a warm frame
    ///    this should be ~0; a high value means the world cache is mismatching/being
    ///    evicted (the dominant fixed per-face cost — a bug).
    ///  * `surf_bypass_baked` — external brush models, which intentionally bypass
    ///    the cache and bake fresh each frame (cheap; one per visible item-box face).
    pub surf_cache_hits: u64,
    pub surf_baked: u64,
    pub surf_bypass_baked: u64,
}

impl RenderStats {
    const ZERO: RenderStats = RenderStats {
        world_ns: 0, submodel_ns: 0, external_ns: 0, alias_ns: 0, particle_ns: 0,
        sprite_ns: 0, viewmodel_ns: 0, faces_total: 0, faces_pvs_culled: 0,
        faces_frustum_culled: 0, faces_drawn: 0, world_tris: 0, world_pixels: 0,
        surf_hits: 0, surf_misses: 0,
        sub_faces_visited: 0, sub_faces_drawn: 0, sub_surf_hits: 0, sub_surf_misses: 0,
        sub_tris: 0, sub_lm_builds: 0,
        world_pvs_ns: 0, world_sort_ns: 0, world_setup_ns: 0, world_light_ns: 0,
        world_surf_ns: 0,
        surf_cache_hits: 0, surf_baked: 0, surf_bypass_baked: 0,
    };
}

/// Enable the render profiler and clear its counters. The NEXT
/// [`render_scene_ext_sprited`] accumulates into [`RenderStats`]; read + disable
/// with [`render_stats_end`]. Intended for the `quaketool` benchmark, not the game.
pub fn render_stats_begin() {
    RENDER_STATS.with(|s| *s.borrow_mut() = RenderStats::ZERO);
    STATS_ON.with(|c| c.set(true));
}

/// Read the accumulated [`RenderStats`] and disable the profiler.
pub fn render_stats_end() -> RenderStats {
    STATS_ON.with(|c| c.set(false));
    RENDER_STATS.with(|s| *s.borrow())
}

/// Whether the render profiler is currently accumulating (cheap `Cell` read).
#[inline]
fn stats_on() -> bool {
    STATS_ON.with(|c| c.get())
}

/// Apply `f` to the live [`RenderStats`] iff profiling is on (no-op otherwise).
#[inline]
fn stat(f: impl FnOnce(&mut RenderStats)) {
    if stats_on() {
        RENDER_STATS.with(|s| f(&mut s.borrow_mut()));
    }
}

/// Maximum baked surface-cache block, in texels. A face larger than this stays on
/// the per-pixel lighting path, so one pathological giant surface can't allocate a
/// multi-MB block; virtually every real id1 face is far smaller.
const SURF_BLOCK_MAX: usize = 1 << 20;

/// A baked surface block handle: `(texels, width, height, surface origin)`.
type SurfBlockRef = (std::rc::Rc<Vec<u8>>, usize, usize, [f32; 2]);

/// Build (and cache) a world face's lit+colormapped surface block (mip 0). Returns
/// the `Rc` handle + dimensions + surface origin, or `None` (caller keeps the
/// per-pixel path) when there is no usable colormap, the face is dynamically lit
/// (`dlit` — the moving dlight can't be baked), the texture is missing, or the
/// block would exceed [`SURF_BLOCK_MAX`].
///
/// FIDELITY: at mip 0 the block is 1:1 with surface texels, so the sampled texture
/// texel is identical to the per-pixel path; only the lighting is sampled at texel
/// centres (then nearest-read per pixel) rather than per screen pixel — which is
/// exactly what Quake's surface cache does (`R_BuildLightMap` + `D_DrawSurfaceBlock8`).
#[allow(clippy::too_many_arguments)]
fn face_surf_block(
    idx: usize,
    face: &crate::bsp::DFace,
    mt: &crate::bsp::MipTex,
    lm: &LightMap,
    colormap: &[u8],
    fp: WorldFingerprint,
    n_faces: usize,
    light_styles: &[f32; LIGHTSTYLES],
    dlit: bool,
    cache_surf: bool,
) -> Option<SurfBlockRef> {
    if dlit || colormap.len() < COLORMAP_LEN {
        return None;
    }
    let (tw, th) = (mt.width as usize, mt.height as usize);
    if tw == 0 || th == 0 || mt.pixels.len() < tw.saturating_mul(th) {
        return None;
    }
    // The block spans the surface's texture-space extent at full resolution. lmw/lmh
    // are extent/16 + 1 luxels, so the texel extent is (lmw-1)*16 + 1.
    let bw = lm.lmw.saturating_sub(1).saturating_mul(16).saturating_add(1);
    let bh = lm.lmh.saturating_sub(1).saturating_mul(16).saturating_add(1);
    let total = bw.checked_mul(bh)?;
    if bw == 0 || bh == 0 || total > SURF_BLOCK_MAX {
        return None;
    }
    // Cache key: the active styles' resolved scales (matching the lightmap cache).
    let mut scales = [0.0f32; crate::bsp::MAXLIGHTMAPS];
    let mut n_styles = 0usize;
    for &style in face.styles.iter() {
        if style == STYLE_NONE {
            break;
        }
        if n_styles < scales.len() {
            scales[n_styles] = light_styles.get(style as usize).copied().unwrap_or(1.0);
        }
        n_styles += 1;
    }

    // The deterministic bake: for each surface texel (i,j) take the tiled base
    // texel, shade it by the lightmap factor at the texel centre, pick the colormap
    // row, and store the final palette index. Identical inputs -> identical bytes,
    // so a cached block is bit-for-bit equal to a fresh one.
    let texmins = lm.texmins;
    let bake = || -> std::rc::Rc<Vec<u8>> {
        let (tmi0, tmi1) = (texmins[0] as i64, texmins[1] as i64);
        let mut block = vec![0u8; total];
        for j in 0..bh {
            let ty = ((tmi1 + j as i64).rem_euclid(th as i64)) as usize;
            let tf = texmins[1] + j as f32;
            for i in 0..bw {
                let tx = ((tmi0 + i as i64).rem_euclid(tw as i64)) as usize;
                let texel = mt.pixels[ty * tw + tx] as usize;
                let bri = lm.factor_at(texmins[0] + i as f32, tf);
                let row = colormap_row(bri);
                block[j * bw + i] = colormap[row * 256 + texel];
            }
        }
        std::rc::Rc::new(block)
    };

    // EXTERNAL brush models (the b_*.bsp ammo/health/explosive boxes) bypass the
    // cache. The game clones each item's `Bsp` per visible instance every frame, so
    // its `WorldFingerprint` (keyed on the `&Bsp` pointer) is different every frame
    // and every instance — it could never produce a cache hit, and routing it
    // through the shared slot would only evict the world's resident cache (the bug
    // this guard prevents). They are tiny (a 6-face box) so an unconditional bake is
    // cheap; bake fresh and return without touching SURF_CACHE.
    if !cache_surf {
        stat(|s| s.surf_bypass_baked += 1);
        return Some((bake(), bw, bh, texmins));
    }

    // CACHED path — the world model and its inline submodels (doors/plats/buttons)
    // all share the one world `Bsp`, so exactly one fingerprint is ever cached at a
    // time. A single slot therefore suffices: it self-invalidates on a changelevel
    // (the new world's `fp` / `n_faces` differ) and holds at most one world's worth
    // of baked blocks, so memory never accumulates across levels.
    SURF_CACHE.with(|c| {
        let mut slot = c.borrow_mut();
        let needs_reset = match slot.as_ref() {
            Some(sc) => sc.fingerprint != fp || sc.entries.len() != n_faces,
            None => true,
        };
        if needs_reset {
            *slot = Some(SurfCache {
                fingerprint: fp,
                entries: vec![None; n_faces],
            });
        }
        let sc = slot.as_mut().expect("just initialised");
        // HIT: same face, same resolved style scales -> reuse the baked block.
        if let Some(e) = sc.entries.get(idx).and_then(|e| e.as_ref()) {
            if e.n_styles == n_styles
                && e.bw == bw
                && e.bh == bh
                && e.style_scales[..n_styles] == scales[..n_styles]
            {
                stat(|s| s.surf_cache_hits += 1);
                return Some((e.block.clone(), e.bw, e.bh, e.texmins));
            }
        }
        // MISS: bake once and store.
        stat(|s| s.surf_baked += 1);
        let block = bake();
        if let Some(e) = sc.entries.get_mut(idx) {
            *e = Some(SurfCacheEntry {
                style_scales: scales,
                n_styles,
                block: block.clone(),
                bw,
                bh,
                texmins,
            });
        }
        Some((block, bw, bh, texmins))
    })
}

/// Compute (and cache) a world-model face's static geometry. Returns a clone of
/// the cached [`FaceGeom`]. The cache is reset whenever the world fingerprint
/// changes (changelevel). `face` must be the corresponding `bsp.faces[idx]`.
fn face_geom_cached(bsp: &Bsp, idx: usize, face: &crate::bsp::DFace) -> FaceGeom {
    let fp = WorldFingerprint::of(bsp);
    GEOM_CACHE.with(|c| {
        let mut slot = c.borrow_mut();
        // (Re)initialise the cache on first use or a world change.
        let needs_reset = match slot.as_ref() {
            Some(gc) => gc.fingerprint != fp || gc.geoms.len() != bsp.faces.len(),
            None => true,
        };
        if needs_reset {
            *slot = Some(GeomCache {
                fingerprint: fp,
                geoms: vec![None; bsp.faces.len()],
            });
        }
        let gc = slot.as_mut().expect("just initialised");
        if let Some(existing) = gc.geoms.get(idx).and_then(|g| g.clone()) {
            return existing;
        }
        // Build it once.
        let mut poly: Vec<Vec3> = Vec::new();
        let ok = face_world_poly(bsp, face, &mut poly);
        let geom = if !ok {
            FaceGeom {
                poly: std::rc::Rc::new(Vec::new()),
                normal: None,
                center: [0.0; 3],
                mins: [0.0; 3],
                maxs: [0.0; 3],
                bad: true,
            }
        } else {
            let normal = face_normal(bsp, face);
            // Centroid: identical accumulate-then-scale to the inline loop.
            let mut center = [0.0f32; 3];
            for v in &poly {
                for k in 0..3 {
                    center[k] += v[k];
                }
            }
            let inv_n = 1.0 / poly.len() as f32;
            for c in &mut center {
                *c *= inv_n;
            }
            // World AABB for the frustum cull.
            let mut mins = [f32::INFINITY; 3];
            let mut maxs = [f32::NEG_INFINITY; 3];
            for v in &poly {
                for k in 0..3 {
                    if v[k] < mins[k] {
                        mins[k] = v[k];
                    }
                    if v[k] > maxs[k] {
                        maxs[k] = v[k];
                    }
                }
            }
            FaceGeom {
                poly: std::rc::Rc::new(poly),
                normal,
                center,
                mins,
                maxs,
                bad: false,
            }
        };
        if let Some(g) = gc.geoms.get_mut(idx) {
            *g = Some(geom.clone());
        }
        geom
    })
}

/// Does any dynamic light in `dlights` actually REACH this face? Mirrors the
/// reach test inside [`add_dynamic_lights`] (`rad = radius - |dist|`, skip if
/// `rad < minlight`) WITHOUT touching luxels, so the lightmap/surface caches
/// can decide whether the static/style buffer is safe to reuse (dlights move
/// every frame, so a touched face must rebuild).
///
/// `dlightbits` is the face's `R_MarkLights` mask (see [`mark_dlights`]): only
/// marked lights are considered, so a light the BSP recursion never carried to
/// this face — e.g. one entirely on the far side of a wall — cannot flag the
/// face as touched, exactly as the C only dlights faces whose
/// `dlightframe == r_dlightframecount`.
///
/// Beyond the mask + plane tests, the light's impact point is tested against
/// the face's texture-space extent with the same `max(sd,td) + min(sd,td)/2`
/// distance estimate `add_dynamic_lights` uses per luxel — i.e. "would this
/// light add light to at least one luxel of this face". This extent test is a
/// PORT-SPECIFIC tightening of the CACHE-PATH decision only: the C keys its
/// surface-cache rebuild on the marking alone (`d_surf.c` checks
/// `surf->dlightframe`) and over-marking there costs only a redundant rebuild,
/// because the C always renders through the surface cache. This port instead
/// flips a "dlit" face from the baked block onto the per-pixel path (which
/// shades visibly differently — the documented texel-center-bake vs bilinear
/// divergence), and `R_MarkLights` marks EVERY face stored on a straddled node
/// regardless of lateral distance (e.g. the whole length of a long floor plane
/// while a rocket flies past one end) — so without the extent test such
/// zero-contribution faces would shimmer for no pixel change (the visible
/// first-frames "pop" this fixed: the start map's distant lava fireballs).
/// Skipping them is sound: a face receiving no luxel light renders identically
/// on the baked path. Conservative at the rim: the extent is the luxel grid's
/// quantized bounds and the distance is the continuous minimum (the port-wide
/// f32 convention; the C truncates `sd`/`td` to int), so a light is never
/// declared "not reaching" when `add_dynamic_lights` would contribute; a
/// missing plane or texinfo falls back to `false`/plane-only (no light is
/// folded without a plane; without texinfo stay conservative).
fn any_dlight_reaches(
    bsp: &Bsp,
    face: &crate::bsp::DFace,
    dlights: &[crate::dlight::DynamicLight],
    dlightbits: u32,
) -> bool {
    if dlights.is_empty() || dlightbits == 0 {
        return false;
    }
    let plane = match (face.planenum as i64)
        .try_into()
        .ok()
        .and_then(|pi: usize| bsp.planes.get(pi))
    {
        Some(p) => p,
        None => return false,
    };
    let normal = plane.normal;
    let texinfo = (face.texinfo as i64)
        .try_into()
        .ok()
        .and_then(|i: usize| bsp.texinfo.get(i));

    // The face's texture-space bounds (the projection CalcSurfaceExtents uses),
    // quantized outward to the 16-unit luxel grid the lightmap actually spans.
    // Inner `None` when the texinfo/vertex walk fails — then plane-only
    // (conservative). Computed LAZILY, only when the first masked light passes
    // the plane test: the common "a light is live somewhere, none near this
    // face's node" frame pays only the mask check, no edge walk.
    let compute_bounds = || -> Option<[f32; 4]> {
        let ti = texinfo?;
        let mut smin = f32::MAX;
        let mut smax = f32::MIN;
        let mut tmin = f32::MAX;
        let mut tmax = f32::MIN;
        let firstedge = face.firstedge as i64;
        if firstedge < 0 || face.numedges < 3 {
            return None;
        }
        for i in 0..face.numedges as i64 {
            let se_index: usize = (firstedge + i).try_into().ok()?;
            let &se = bsp.surfedges.get(se_index)?;
            let (edge_index, slot): (usize, usize) = if se >= 0 {
                (se as usize, 0)
            } else {
                ((se as i64).checked_neg()? as usize, 1)
            };
            let vid = *bsp.edges.get(edge_index)?.v.get(slot)? as usize;
            let p = bsp.vertexes.get(vid)?.point;
            let s = p[0] * ti.vecs[0][0] + p[1] * ti.vecs[0][1] + p[2] * ti.vecs[0][2]
                + ti.vecs[0][3];
            let t = p[0] * ti.vecs[1][0] + p[1] * ti.vecs[1][1] + p[2] * ti.vecs[1][2]
                + ti.vecs[1][3];
            smin = smin.min(s);
            smax = smax.max(s);
            tmin = tmin.min(t);
            tmax = tmax.max(t);
        }
        Some([
            (smin / 16.0).floor() * 16.0,
            (smax / 16.0).ceil() * 16.0,
            (tmin / 16.0).floor() * 16.0,
            (tmax / 16.0).ceil() * 16.0,
        ])
    };
    let mut bounds: Option<Option<[f32; 4]>> = None;

    // The same projection helper for the light's impact point.
    let project = |p: [f32; 3], v: &[f32; 4]| p[0] * v[0] + p[1] * v[1] + p[2] * v[2] + v[3];

    for (lnum, dl) in dlights.iter().enumerate() {
        // Same mask skip as `add_dynamic_lights` (C `surf->dlightbits & (1<<lnum)`).
        if lnum >= u32::BITS as usize || dlightbits & (1u32 << lnum) == 0 {
            continue;
        }
        let dist = dot(dl.origin, normal) - plane.dist;
        let rad = dl.radius - dist.abs();
        if rad < dl.minlight {
            continue;
        }
        let b = *bounds.get_or_insert_with(compute_bounds);
        let (Some([smin, smax, tmin, tmax]), Some(ti)) = (b, texinfo) else {
            return true; // plane reached; no extent info -> conservative
        };
        // Project the light onto the plane and into texture space, then measure
        // the dist2 estimate to the nearest point of the face's luxel extent.
        let impact = [
            dl.origin[0] - normal[0] * dist,
            dl.origin[1] - normal[1] * dist,
            dl.origin[2] - normal[2] * dist,
        ];
        let ls = project(impact, &ti.vecs[0]);
        let lt = project(impact, &ti.vecs[1]);
        let sd = (smin - ls).max(ls - smax).max(0.0);
        let td = (tmin - lt).max(lt - tmax).max(0.0);
        let dist2 = if sd > td { sd + td * 0.5 } else { td + sd * 0.5 };
        if dist2 < rad - dl.minlight {
            return true;
        }
    }
    false
}

/// `R_PushDlights` (`r_light.c`): compute every face's `surf->dlightbits` mask
/// for this frame by running the `R_MarkLights` BSP recursion from `headnode`
/// once per live light (`dlights[i]` marks bit `1 << i`).
///
/// `bits` is a caller-owned scratch vector, cleared and resized to
/// `bsp.faces.len()` (so once it has grown to the map's face count the per-frame
/// marking allocates nothing). With no live lights it is left EMPTY — every
/// lookup then reads 0 ("no light marked"), and the zero-fill is skipped.
///
/// The C resets stale masks with the `dlightframe != r_dlightframecount` check;
/// zero-filling the scratch each frame is the equivalent here.
///
/// FALLBACK: a `Bsp` with no node tree (synthetic fixtures like [`demo_room`])
/// has nothing to recurse, so every face is marked with [`ALL_DLIGHT_BITS`] and
/// the per-light distance test in [`add_dynamic_lights`] remains the only gate —
/// the pre-gating behaviour. Real maps always carry a node tree.
fn mark_dlights(
    bsp: &Bsp,
    headnode: i32,
    dlights: &[crate::dlight::DynamicLight],
    bits: &mut Vec<u32>,
) {
    bits.clear();
    if dlights.is_empty() {
        return;
    }
    if bsp.nodes.is_empty() {
        bits.resize(bsp.faces.len(), ALL_DLIGHT_BITS);
        return;
    }
    bits.resize(bsp.faces.len(), 0);
    for (i, dl) in dlights.iter().take(u32::BITS as usize).enumerate() {
        // In a well-formed tree each node is visited at most once per light, so
        // a budget of `nodes.len()` visits never truncates a legitimate walk; it
        // only stops a malformed (cyclic) node graph from recursing forever.
        let mut budget = bsp.nodes.len();
        mark_lights_r(bsp, dl, 1u32 << i, headnode, &mut budget, bits);
    }
}

/// `R_MarkLights` (`r_light.c`): descend the BSP from `node`, OR-ing `bit` into
/// `bits[face]` for every surface the light's sphere reaches through the tree.
///
/// At each node: `dist = dot(light.origin, plane.normal) - plane.dist`. When the
/// whole sphere is on one side (`dist > radius` / `dist < -radius`) only that
/// child subtree is descended — nothing behind a plane the light does not reach
/// can be marked, which is what stops a dynamic light lighting faces through a
/// wall. When the sphere straddles the plane, the surfaces stored on this node
/// (which lie on that plane) are marked and both children are descended.
///
/// A negative `node` is a leaf reference (`-(leaf+1)`) and ends the descent,
/// matching the C `if (node->contents < 0) return;`. Out-of-range node/plane
/// indices return harmlessly; `budget` bounds total node visits (see
/// [`mark_dlights`]).
fn mark_lights_r(
    bsp: &Bsp,
    light: &crate::dlight::DynamicLight,
    bit: u32,
    node: i32,
    budget: &mut usize,
    bits: &mut [u32],
) {
    if node < 0 || *budget == 0 {
        return;
    }
    *budget -= 1;
    let node_rec = match usize::try_from(node).ok().and_then(|ni| bsp.nodes.get(ni)) {
        Some(n) => n,
        None => return,
    };
    let plane = match usize::try_from(node_rec.planenum)
        .ok()
        .and_then(|pi| bsp.planes.get(pi))
    {
        Some(p) => p,
        None => return,
    };

    let dist = dot(light.origin, plane.normal) - plane.dist;

    if dist > light.radius {
        mark_lights_r(bsp, light, bit, node_rec.children[0] as i32, budget, bits);
        return;
    }
    if dist < -light.radius {
        mark_lights_r(bsp, light, bit, node_rec.children[1] as i32, budget, bits);
        return;
    }

    // Mark the polygons stored on this node (they lie on the straddled plane).
    let first = node_rec.firstface as usize;
    for entry in bits.iter_mut().skip(first).take(node_rec.numfaces as usize) {
        *entry |= bit;
    }

    mark_lights_r(bsp, light, bit, node_rec.children[0] as i32, budget, bits);
    mark_lights_r(bsp, light, bit, node_rec.children[1] as i32, budget, bits);
}

/// The lightmap for a world-model face, going through the surface cache.
///
/// Behaviour, by case:
///  * **Static borrow** (`face_lightmap_dyn` returns `Luxels::Static`, the
///    common steady style-0-at-neutral case) — returned as-is, no caching: it
///    already borrows the BSP bytes and is byte-identical to before.
///  * **Dynamic light reaches the face** — rebuilt EVERY frame via
///    `face_lightmap_dyn` (dlights move). The result is NOT stored, and any
///    previously cached entry for this face is dropped, so the dlight is never
///    silently lost on a later frame.
///  * **Owned combine, no dlight** (animated styles) — keyed by the resolved
///    style scale values. On a hit with matching scales the cached `Vec<f32>` is
///    cloned into a fresh `LightMap` (bit-identical to a rebuild — the combine
///    is deterministic). On a miss it is rebuilt and stored.
///
/// The cache is reset on a world fingerprint change (changelevel), since it is
/// keyed by face index.
fn face_lightmap_world_cached<'a>(
    bsp: &'a Bsp,
    idx: usize,
    face: &crate::bsp::DFace,
    world_poly: &[Vec3],
    light_styles: &[f32; LIGHTSTYLES],
    dlights: &[crate::dlight::DynamicLight],
    // The face's `R_MarkLights` mask for this frame (see [`mark_dlights`]).
    dlightbits: u32,
) -> Option<LightMap<'a>> {
    // Resolve the active styles' SCALE values (cache key) once.
    let mut scales = [0.0f32; crate::bsp::MAXLIGHTMAPS];
    let mut n_styles = 0usize;
    for &style in face.styles.iter() {
        if style == STYLE_NONE {
            break;
        }
        scales[n_styles] = light_styles.get(style as usize).copied().unwrap_or(1.0);
        n_styles += 1;
    }

    let dlit = any_dlight_reaches(bsp, face, dlights, dlightbits);

    let fp = WorldFingerprint::of(bsp);

    if dlit {
        // A dlight touches this face: rebuild every frame and DROP any cached
        // entry (so we never reuse a stale, dlight-free buffer next frame, and
        // never bake a moving dlight into the cache).
        LIGHT_CACHE.with(|c| {
            let mut slot = c.borrow_mut();
            if let Some(lc) = slot.as_mut() {
                if lc.fingerprint == fp {
                    if let Some(e) = lc.entries.get_mut(idx) {
                        *e = None;
                    }
                }
            }
        });
        return face_lightmap_dyn(bsp, face, world_poly, light_styles, dlights, dlightbits);
    }

    // No dlight: try the cache.
    let cached = LIGHT_CACHE.with(|c| {
        let mut slot = c.borrow_mut();
        let needs_reset = match slot.as_ref() {
            Some(lc) => lc.fingerprint != fp || lc.entries.len() != bsp.faces.len(),
            None => true,
        };
        if needs_reset {
            *slot = Some(LightCache {
                fingerprint: fp,
                entries: vec![None; bsp.faces.len()],
            });
        }
        let lc = slot.as_mut().expect("just initialised");
        match lc.entries.get(idx).and_then(|e| e.as_ref()) {
            Some(e)
                if e.n_styles == n_styles
                    && e.style_scales[..n_styles] == scales[..n_styles] =>
            {
                // HIT: clone the stored combined luxels (deterministic build ->
                // bit-identical to rebuilding).
                Some(LightCacheEntry {
                    style_scales: e.style_scales,
                    n_styles: e.n_styles,
                    luxels: e.luxels.clone(),
                    lmw: e.lmw,
                    lmh: e.lmh,
                    texmins: e.texmins,
                })
            }
            _ => None,
        }
    });

    if let Some(e) = cached {
        return Some(LightMap {
            luxels: Luxels::Owned(e.luxels),
            lmw: e.lmw,
            lmh: e.lmh,
            texmins: e.texmins,
        });
    }

    // MISS: build fresh (no dlights -> the result is the pure static/style
    // combine), then cache it if it is the owned combine.
    let built = face_lightmap_dyn(bsp, face, world_poly, light_styles, &[], 0)?;
    if let Luxels::Owned(ref v) = built.luxels {
        let mut style_scales = [0.0f32; crate::bsp::MAXLIGHTMAPS];
        style_scales[..n_styles].copy_from_slice(&scales[..n_styles]);
        let entry = LightCacheEntry {
            style_scales,
            n_styles,
            luxels: v.clone(),
            lmw: built.lmw,
            lmh: built.lmh,
            texmins: built.texmins,
        };
        LIGHT_CACHE.with(|c| {
            let mut slot = c.borrow_mut();
            if let Some(lc) = slot.as_mut() {
                if lc.fingerprint == fp {
                    if let Some(e) = lc.entries.get_mut(idx) {
                        *e = Some(entry);
                    }
                }
            }
        });
    }
    Some(built)
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
    colormap: Option<&[u8]>,
) {
    // The near plane lives in `clip_poly_near` (`NEAR_PLANE`); this pass clips the
    // polygon to it rather than dropping any face that touches it.
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

    // Sub-phase profiling: accumulate ns into locals (cheap), flush to RenderStats
    // at function end. Only the `prof` path pays the Instant cost; the live game
    // (stats off) skips all of it.
    let prof = stats_on();
    let mut t_sort: u64 = 0;
    let mut t_light: u64 = 0;
    let mut t_surf: u64 = 0;
    let _t_pvs = prof.then(std::time::Instant::now);

    // PVS culling: a per-face visibility mask for the camera's leaf, or `None`
    // when there is no usable PVS (no vis lump, solid/outside leaf, malformed) —
    // in which case every face is drawn (the pre-PVS behaviour).
    let visible_face = compute_visible_faces(bsp, cam.pos);

    // View frustum (4 sides + near) for `R_CullBox`-style AABB rejection. Built
    // once per frame from the camera + aspect; see [`Frustum::from_camera`] for
    // the exact match to the rasteriser's screen rectangle (so it never culls a
    // face that could draw a pixel).
    let frustum = Frustum::from_camera(cam, w, h);
    let t_pvs = _t_pvs.map(|t| t.elapsed().as_nanos() as u64).unwrap_or(0);

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

    let mut views: Vec<VView> = Vec::new();
    let mut proj: Vec<ProjT> = Vec::new();
    // Reused near-clip output buffer (cleared per face) so the per-face clip
    // allocates nothing on the common in-front-of-near fast path.
    let mut clipped: Vec<VView> = Vec::new();

    // World fingerprint + face count for the lit-surface cache (keyed per face).
    let fp = WorldFingerprint::of(bsp);
    let n_faces = bsp.faces.len();

    // `R_PushDlights` (`r_light.c`): mark every face each live dynamic light
    // reaches by recursing the BSP from the world model's root node. A face's
    // mask gates which lights `add_dynamic_lights` may fold into it, so a light
    // whose sphere never crosses into a region (e.g. the far side of a wall)
    // cannot brighten faces there. The scratch is thread-local and reused; with
    // no live dlights it stays empty and every face reads mask 0.
    let mut dlight_bits = DLIGHT_BITS_SCRATCH.with(|b| std::mem::take(&mut *b.borrow_mut()));
    let world_headnode = bsp
        .models
        .first()
        .and_then(|m| m.headnode.first().copied())
        .unwrap_or(0);
    mark_dlights(bsp, world_headnode, dlights, &mut dlight_bits);

    // FRONT-TO-BACK ORDER. With a z-buffer the final image is identical for ANY draw
    // order (the nearest surface always wins the depth test), but drawing near faces
    // FIRST lets the z-test reject occluded pixels BEFORE the per-pixel shading
    // (block read + framebuffer write) — cutting the ~1.9x world overdraw the
    // profiler measured. Sort visible faces by squared centroid distance (ascending);
    // bad-geom faces sort last (they draw nothing). This reuses the per-face geom
    // cache, so the ordering pass also warms it for the draw loop below. (A true BSP
    // back-to-front/front-to-back walk would be marginally better, but centroid sort
    // captures the bulk of the win for walls and is far simpler / output-identical.)
    let _t_sort = prof.then(std::time::Instant::now);
    let mut world_order: Vec<(f32, usize)> =
        Vec::with_capacity(world_end.saturating_sub(world_first));
    for fi in world_first..world_end {
        let key = match bsp.faces.get(fi) {
            Some(face) => {
                let g = face_geom_cached(bsp, fi, face);
                if g.bad {
                    f32::MAX
                } else {
                    let d = sub(g.center, cam.pos);
                    dot(d, d)
                }
            }
            None => f32::MAX,
        };
        world_order.push((key, fi));
    }
    world_order.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    if let Some(t) = _t_sort { t_sort += t.elapsed().as_nanos() as u64; }

    let _t_body = prof.then(std::time::Instant::now);
    for &(_, face_index) in &world_order {
        let face = match bsp.faces.get(face_index) {
            Some(f) => f,
            None => continue,
        };
        stat(|s| s.faces_total += 1);
        // Skip faces outside the potentially-visible set. A missing mask entry
        // (or no mask at all) means "draw" — culling never removes a face it is
        // unsure about.
        if let Some(mask) = &visible_face {
            if !mask.get(face_index).copied().unwrap_or(true) {
                stat(|s| s.faces_pvs_culled += 1);
                continue;
            }
        }

        // Static per-face geometry (poly / normal / centroid / AABB), built once
        // for the world model and reused every frame. `bad` reproduces the
        // original `face_world_poly` early-out exactly.
        let geom = face_geom_cached(bsp, face_index, face);
        if geom.bad {
            continue;
        }

        // FRUSTUM CULL (R_CullBox): reject faces whose static world AABB is fully
        // outside the view, BEFORE projection / lightmap / raster. Conservative —
        // a face touching the view survives. This precedes the normal/backface
        // checks; a culled face draws nothing, so the output is unchanged.
        if frustum.culls(geom.mins, geom.maxs) {
            stat(|s| s.faces_frustum_culled += 1);
            continue;
        }

        let world_poly: &[Vec3] = &geom.poly;
        let normal = match geom.normal {
            Some(n) => n,
            None => continue,
        };

        // Face center for the backface cull (cached centroid).
        let center = geom.center;
        if dot(normal, sub(center, cam.pos)) >= 0.0 {
            continue;
        }

        // texinfo (s/t axes) and its miptexture. For a `+`-prefixed animated
        // texture, the miptex actually sampled is chosen by time via
        // `R_TextureAnimation` (the world entity's frame is 0, so it never takes
        // the alternate cycle). The (s,t) axes still come from the texinfo, which
        // are shared by every frame of the animation.
        let ti = (face.texinfo as i64)
            .try_into()
            .ok()
            .and_then(|i: usize| bsp.texinfo.get(i));
        let tex = ti.and_then(|t| {
            let mi: usize = t.miptex.try_into().ok()?;
            let anim_mi = texture_animation(bsp, mi, 0, time);
            bsp.textures.get(anim_mi).and_then(|o| o.as_ref())
        });

        // Classify the surface (liquid / sky / wall) by its miptex name so the
        // animated special surfaces route to the warp/scroll sampler. Liquids
        // and sky are fullbright and NOT lightmapped, so only walls compute a
        // baked static lightmap.
        let kind = tex.map(|mt| classify_surface(&mt.name)).unwrap_or(SurfKind::Normal);
        // This face's `R_MarkLights` mask (0 = no dynamic light reaches it).
        let face_dlightbits = dlight_bits.get(face_index).copied().unwrap_or(0);
        let _t_l = prof.then(std::time::Instant::now);
        let lightmap = if kind == SurfKind::Normal {
            // Goes through the lightmap SURFACE CACHE (R_BuildLightMap cache):
            // reuses the combined luxel buffer while the resolved style scales
            // are unchanged and no dynamic light touches the face. The cached
            // buffer is bit-identical to a fresh build.
            face_lightmap_world_cached(bsp, face_index, face, world_poly, light_styles, dlights, face_dlightbits)
        } else {
            None
        };
        if let Some(t) = _t_l { t_light += t.elapsed().as_nanos() as u64; }
        let mode = match kind {
            SurfKind::Normal => SurfaceMode::Normal,
            SurfKind::Turb => SurfaceMode::Turb { turb, time },
            SurfKind::Sky => SurfaceMode::Sky {
                time,
                view: SkyView {
                    forward,
                    right,
                    up,
                    cx,
                    cy,
                    longest: (w.max(h)) as f32,
                },
            },
        };

        // Build the view-space polygon (vx,vy,vz,s,t per world vertex), then clip
        // it against the near plane. A face fully in front is returned unchanged
        // (no regression); a face fully behind yields < 3 verts and is skipped;
        // a straddling face is clipped to `vz == NEAR` and rasterised normally.
        views.clear();
        for v in world_poly {
            let rel = sub(*v, cam.pos);
            let vz = dot(rel, forward);
            let vx = dot(rel, right);
            let vy = dot(rel, up);
            let (s, t) = match ti {
                Some(ti) => (
                    v[0] * ti.vecs[0][0] + v[1] * ti.vecs[0][1] + v[2] * ti.vecs[0][2] + ti.vecs[0][3],
                    v[0] * ti.vecs[1][0] + v[1] * ti.vecs[1][1] + v[2] * ti.vecs[1][2] + ti.vecs[1][3],
                ),
                None => (0.0, 0.0),
            };
            views.push(VView { vx, vy, vz, s, t });
        }
        clip_poly_near_into(&views, &mut clipped);
        if clipped.len() < 3 {
            continue;
        }
        proj.clear();
        for vv in &clipped {
            proj.push(ProjT {
                x: cx + focal * vv.vx / vv.vz,
                y: cy - focal * vv.vy / vv.vz,
                vz: vv.vz,
                s: vv.s,
                t: vv.t,
            });
        }

        let lambert = dot(normal, light_dir).max(0.0);
        let shade = (0.5 + 0.5 * lambert).min(1.0);

        match tex {
            Some(mt) if !mt.pixels.is_empty() && mt.width > 0 && mt.height > 0 => {
                let (tw, th) = (mt.width as usize, mt.height as usize);
                let v0 = proj[0];
                // Lit SURFACE CACHE (Quake d_surf.c): a lightmapped wall with a
                // colormap and no reaching dynamic light bakes texture*lightmap*
                // colormap into a per-surface block ONCE, then reads one byte per
                // pixel. Turb/sky/dynamically-lit/colormap-less surfaces keep the
                // per-pixel path (raster_triangle_tex).
                stat(|s| { s.faces_drawn += 1; s.world_tris += (proj.len() - 2) as u64; });
                let _t_s = prof.then(std::time::Instant::now);
                let surf = if matches!(mode, SurfaceMode::Normal) {
                    match (lightmap.as_ref(), colormap) {
                        (Some(lm), Some(cm)) => {
                            let dlit = any_dlight_reaches(bsp, face, dlights, face_dlightbits);
                            // World model: cacheable (stable `Bsp` across frames).
                            face_surf_block(
                                face_index, face, mt, lm, cm, fp, n_faces, light_styles, dlit, true,
                            )
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(t) = _t_s { t_surf += t.elapsed().as_nanos() as u64; }
                match surf {
                    Some((block, bw, bh, tmins)) => {
                        stat(|s| s.surf_hits += 1);
                        for i in 1..proj.len() - 1 {
                            raster_triangle_cached(
                                image, zbuf, v0, proj[i], proj[i + 1], &block, bw, bh, tmins,
                                palette,
                            );
                        }
                    }
                    None => {
                        stat(|s| s.surf_misses += 1);
                        for i in 1..proj.len() - 1 {
                            raster_triangle_tex(
                                image, zbuf, v0, proj[i], proj[i + 1],
                                &mt.pixels, tw, th, palette, shade, lightmap.as_ref(), mode,
                                colormap,
                            );
                        }
                    }
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
                                colormap,
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

    // Return the marking scratch for the next pass/frame (keeps its capacity).
    DLIGHT_BITS_SCRATCH.with(|b| *b.borrow_mut() = dlight_bits);

    // Flush sub-phase timers. `setup` = whole draw-loop body minus the measured
    // lightmap + surf-block calls (so it captures geom-cache fetch, the culls,
    // texinfo/classify, near-clip + projection, and — at high res — the raster).
    if prof {
        let body = _t_body.map(|t| t.elapsed().as_nanos() as u64).unwrap_or(0);
        let t_setup = body.saturating_sub(t_light).saturating_sub(t_surf);
        stat(|s| {
            s.world_pvs_ns += t_pvs;
            s.world_sort_ns += t_sort;
            s.world_setup_ns += t_setup;
            s.world_light_ns += t_light;
            s.world_surf_ns += t_surf;
        });
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
    colormap: Option<&[u8]>,
    ent_frame: i32,
    // Whether this submodel's `bsp` is the stable world `Bsp` (inline submodels —
    // doors/plats/buttons — share it, so their baked surface blocks are worth
    // caching) or an ephemeral per-instance clone (external b_*.bsp item boxes,
    // which re-clone every frame and must bypass the surface cache; see
    // [`face_surf_block`]).
    cache_surf: bool,
) {
    // The near plane lives in `clip_poly_near` (`NEAR_PLANE`); this pass clips the
    // polygon to it rather than dropping any face that touches it.
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

    // World fingerprint + face count for the lit-surface cache. A submodel's (s,t)
    // and lightmap come from its LOCAL (origin-independent) polygon, so the baked
    // texture*lightmap*colormap block is identical wherever the door/plat sits —
    // cacheable by GLOBAL face index just like the world pass (the world's and the
    // submodels' face ranges are disjoint, so there is no key collision).
    let fp = WorldFingerprint::of(bsp);
    let n_faces = bsp.faces.len();

    // `R_MarkLights` over the submodel's OWN subtree, as the C marks bmodels from
    // `clmodel->nodes + clmodel->hulls[0].firstclipnode` (`R_DrawBEntitiesOnList`,
    // `r_main.c`) — the submodel's `headnode[0]`. The subtree's node face lists
    // only reference this submodel's faces, so the mask is per-face like the world
    // pass. Marked with the entity-LOCAL light origins, consistent with the local
    // per-luxel lighting above (the subtree planes are in model-local space).
    let mut dlight_bits = DLIGHT_BITS_SCRATCH.with(|b| std::mem::take(&mut *b.borrow_mut()));
    let sub_headnode = m.headnode.first().copied().unwrap_or(0);
    mark_dlights(bsp, sub_headnode, &local_dlights, &mut dlight_bits);

    // `world_poly` holds origin-SHIFTED vertices (for projection); we keep the
    // LOCAL vertices separately for (s,t) and the lightmap.
    let mut local_poly: Vec<Vec3> = Vec::new();
    let mut world_poly: Vec<Vec3> = Vec::new();
    let mut views: Vec<VView> = Vec::new();
    let mut proj: Vec<ProjT> = Vec::new();
    // Reused near-clip output buffer (cleared per face); zero per-face alloc.
    let mut clipped: Vec<VView> = Vec::new();

    for face_index in f0..end {
        let face = match bsp.faces.get(face_index) {
            Some(f) => f,
            None => continue,
        };
        // Every submodel face VISITED this frame (before backface/near cull). The
        // gap between this and `sub_faces_drawn` is the per-frame setup cost
        // (face_world_poly reconstruction etc.) paid on faces that never draw.
        stat(|s| s.sub_faces_visited += 1);

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
        for c in &mut center {
            *c *= inv_n;
        }
        if dot(normal, sub(center, cam.pos)) >= 0.0 {
            continue;
        }

        // texinfo (s/t axes) and its miptexture. Animated (`+`-prefixed) textures
        // on a brush submodel cycle by time exactly as the world does
        // (`R_TextureAnimation`); `ent_frame` selects the `+a..+j` alternate
        // (switch) cycle when the brush entity is activated (e.g. a pressed button
        // turning green), else the primary `+0..+9` cycle.
        let ti = (face.texinfo as i64)
            .try_into()
            .ok()
            .and_then(|i: usize| bsp.texinfo.get(i));
        let tex = ti.and_then(|t| {
            let mi: usize = t.miptex.try_into().ok()?;
            let anim_mi = texture_animation(bsp, mi, ent_frame, time);
            bsp.textures.get(anim_mi).and_then(|o| o.as_ref())
        });

        // Classify the surface (liquid / sky / wall) by its miptex name. Liquids
        // and sky are fullbright and NOT lightmapped; only walls compute a
        // lightmap (from the LOCAL polygon — texinfo extents are origin-independent).
        let kind = tex.map(|mt| classify_surface(&mt.name)).unwrap_or(SurfKind::Normal);
        // This face's `R_MarkLights` mask (0 = no dynamic light reaches it).
        let face_dlightbits = dlight_bits.get(face_index).copied().unwrap_or(0);
        let lightmap = if kind == SurfKind::Normal {
            stat(|s| s.sub_lm_builds += 1);
            face_lightmap_dyn(bsp, face, &local_poly, light_styles, &local_dlights, face_dlightbits)
        } else {
            None
        };
        let mode = match kind {
            SurfKind::Normal => SurfaceMode::Normal,
            SurfKind::Turb => SurfaceMode::Turb { turb, time },
            SurfKind::Sky => SurfaceMode::Sky {
                time,
                view: SkyView {
                    forward,
                    right,
                    up,
                    cx,
                    cy,
                    longest: (w.max(h)) as f32,
                },
            },
        };

        // Build the view-space polygon from the SHIFTED vertices (for vx/vy/vz)
        // but with (s,t) from the LOCAL (pre-shift) vertex, then near-clip it.
        // A length mismatch between the shifted and local polygons is treated as
        // a malformed face and skips it (matching the old defensive break).
        views.clear();
        let mut bad = false;
        for (vi, vw) in world_poly.iter().enumerate() {
            let rel = sub(*vw, cam.pos);
            let vz = dot(rel, forward);
            let vx = dot(rel, right);
            let vy = dot(rel, up);
            // (s,t) from the local (pre-shift) vertex coordinate.
            let vl = match local_poly.get(vi) {
                Some(v) => *v,
                None => {
                    bad = true;
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
            views.push(VView { vx, vy, vz, s, t });
        }
        if bad {
            continue;
        }
        clip_poly_near_into(&views, &mut clipped);
        if clipped.len() < 3 {
            continue;
        }
        proj.clear();
        for vv in &clipped {
            proj.push(ProjT {
                x: cx + focal * vv.vx / vv.vz,
                y: cy - focal * vv.vy / vv.vz,
                vz: vv.vz,
                s: vv.s,
                t: vv.t,
            });
        }

        let lambert = dot(normal, light_dir).max(0.0);
        let shade = (0.5 + 0.5 * lambert).min(1.0);

        match tex {
            Some(mt) if !mt.pixels.is_empty() && mt.width > 0 && mt.height > 0 => {
                let (tw, th) = (mt.width as usize, mt.height as usize);
                let v0 = proj[0];
                stat(|s| { s.sub_faces_drawn += 1; s.sub_tris += (proj.len() - 2) as u64; });
                // Lit SURFACE CACHE for submodels (doors/plats/buttons) — same as the
                // world pass. Gated on `ent_frame == 0`: an ACTIVATED brush entity
                // (frame != 0) samples the alternate (+a..+j) texture cycle — a
                // different miptex than the baked block — so it falls back to the
                // per-pixel path. Turb/sky/dlit/colormap-less faces also fall back.
                let surf = if ent_frame == 0 && matches!(mode, SurfaceMode::Normal) {
                    match (lightmap.as_ref(), colormap) {
                        (Some(lm), Some(cm)) => {
                            let dlit = any_dlight_reaches(bsp, face, &local_dlights, face_dlightbits);
                            face_surf_block(
                                face_index, face, mt, lm, cm, fp, n_faces, light_styles, dlit,
                                cache_surf,
                            )
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                match surf {
                    Some((block, bw, bh, tmins)) => {
                        stat(|s| s.sub_surf_hits += 1);
                        for i in 1..proj.len() - 1 {
                            raster_triangle_cached(
                                image, zbuf, v0, proj[i], proj[i + 1], &block, bw, bh, tmins,
                                palette,
                            );
                        }
                    }
                    None => {
                        stat(|s| s.sub_surf_misses += 1);
                        for i in 1..proj.len() - 1 {
                            raster_triangle_tex(
                                image, zbuf, v0, proj[i], proj[i + 1],
                                &mt.pixels, tw, th, palette, shade, lightmap.as_ref(), mode,
                                colormap,
                            );
                        }
                    }
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
                                colormap,
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

    // Return the marking scratch for the next pass/frame (keeps its capacity).
    DLIGHT_BITS_SCRATCH.with(|b| *b.borrow_mut() = dlight_bits);
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
/// `frame` selects which pose to draw (see [`mdl_frame_verts`]); an out-of-range
/// frame resets to 0 (matching `R_AliasSetupFrame`), so any value is safe.
///
/// `skinnum` is the per-entity skin index (`currententity->skinnum`): a model
/// with multiple skins (or a skin group) uses it to pick / animate its skin, so
/// e.g. a damaged or team-coloured variant renders. Out-of-range resets to 0.
/// Defaults to `0` via [`ModelInstance::with_frame`] for callers that do not yet
/// track per-entity skins.
///
/// Group-frame (`ALIAS_GROUP`) and group-skin (`ALIAS_SKIN_GROUP`) animation is
/// driven by the **scene `time`** passed to [`render_scene_ext`] (not a
/// per-instance field), so existing callers animate for free as game time
/// advances. `R_AliasSetupFrame` / `R_AliasSetupSkin` select the sub-frame /
/// sub-skin whose interval window contains that time.
pub struct ModelInstance<'a> {
    pub mdl: &'a crate::mdl::Mdl,
    pub origin: Vec3,
    pub yaw: f32,
    /// Entity pitch (`angles[PITCH]`, degrees, +down as QuakeC stores it). Drives
    /// flying projectiles (rockets/grenades/spikes point along their flight path).
    /// `0.0` keeps the model upright and renders bit-identically to yaw-only.
    pub pitch: f32,
    /// Entity roll (`angles[ROLL]`, degrees). `0.0` for upright models.
    pub roll: f32,
    pub frame: usize,
    pub color: [u8; 3],
    /// Per-entity skin index (`currententity->skinnum`); out-of-range -> 0.
    ///
    /// NOTE: this is a `Default`-able field; callers that build a [`ModelInstance`]
    /// with a struct literal must set it (use `skinnum: 0` when the entity's skin
    /// is not tracked). The [`ModelInstance::with_frame`] / [`ModelInstance::new`]
    /// constructors default it to 0.
    pub skinnum: i32,
}

impl<'a> ModelInstance<'a> {
    /// Build a [`ModelInstance`] with `skinnum`/`pitch`/`roll` defaulted to 0 — the
    /// common case for callers that only track yaw. Equivalent to the old literal.
    pub fn with_frame(
        mdl: &'a crate::mdl::Mdl,
        origin: Vec3,
        yaw: f32,
        frame: usize,
        color: [u8; 3],
    ) -> ModelInstance<'a> {
        ModelInstance { mdl, origin, yaw, pitch: 0.0, roll: 0.0, frame, color, skinnum: 0 }
    }

    /// Build a [`ModelInstance`] specifying the per-entity `skinnum` (pitch/roll 0).
    pub fn new(
        mdl: &'a crate::mdl::Mdl,
        origin: Vec3,
        yaw: f32,
        frame: usize,
        color: [u8; 3],
        skinnum: i32,
    ) -> ModelInstance<'a> {
        ModelInstance { mdl, origin, yaw, pitch: 0.0, roll: 0.0, frame, color, skinnum }
    }
}

/// Resolve the vertices of pose `frame` at game `time` for an [`Mdl`], delegating
/// to [`crate::mdl::Mdl::frame_pose`] (the `R_AliasSetupFrame` port).
///
/// `frame` (a `usize` here) is range-checked there: an out-of-range frame **resets
/// to 0** (matching the C, which does NOT clamp to the last frame). A
/// [`crate::mdl::Frame::Group`] now ANIMATES — its sub-pose is selected by `time`
/// against the group's intervals — so monster/torch group-frame models cycle
/// instead of freezing on the first sub-pose.
///
/// Returns `None` only when the model has no frames at all (or a group is empty).
fn mdl_frame_verts(
    mdl: &crate::mdl::Mdl,
    frame: usize,
    time: f32,
) -> Option<&[crate::mdl::TriVertex]> {
    // `usize -> i32`: a frame beyond `i32::MAX` is treated as out of range (-> 0),
    // exactly as `frame_pose` would do for any out-of-range index.
    let f = i32::try_from(frame).unwrap_or(-1);
    mdl.frame_pose(f, time)
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

/// Resolve the texturing skin for an alias model at entity skin index `skinnum`
/// and game `time`, ported from `R_AliasSetupSkin` (`r_alias.c`): the skin is
/// chosen by the entity's `skinnum` (out of range -> 0), and an
/// `ALIAS_SKIN_GROUP` skin ANIMATES — the image is selected by `time` against
/// the group's intervals (via [`crate::mdl::Mdl::skin_image`]). The dimensions
/// are always the header's `skinwidth`/`skinheight`.
///
/// Returns `None` — so the caller falls back to the flat-colour path for the
/// whole model — when there is no skin, the dimensions are non-positive, the
/// pixel/dimension product overflows, or the pixel buffer is shorter than
/// `skinwidth * skinheight`.
fn mdl_skin(mdl: &crate::mdl::Mdl, skinnum: i32, time: f32) -> Option<ModelSkin<'_>> {
    let pixels: &[u8] = mdl.skin_image(skinnum, time)?;
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
#[allow(clippy::too_many_arguments)]
fn draw_alias_model(
    image: &mut Image,
    zbuf: &mut [f32],
    bsp: &Bsp,
    cam: &Camera,
    inst: &ModelInstance,
    w: usize,
    h: usize,
    palette: &[[u8; 3]; 256],
    dlights: &[crate::dlight::DynamicLight],
    light_styles: &[f32; LIGHTSTYLES],
    time: f32,
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

    // FAITHFULNESS (alias lighting): instead of a fixed directional Lambert, the C
    // (`R_DrawEntitiesOnList`) lights an alias model from the WORLD: it samples
    // `R_LightPoint(origin)` for the baked light at the model's feet, then adds
    // any dynamic light whose radius reaches the origin (`add = radius - dist`).
    // We compute the same scalar here, once per model. The sampled world light is
    // 0..255; a `+`-prefixed style flicker is folded in via `light_styles`. We map
    // that scalar to a per-model brightness `model_light` and modulate it by a mild
    // per-triangle Lambert so silhouettes still read.
    let world_light = r_light_point(bsp, inst.origin, light_styles);
    let mut ambient = world_light;
    for dl in dlights {
        let dx = inst.origin[0] - dl.origin[0];
        let dy = inst.origin[1] - dl.origin[1];
        let dz = inst.origin[2] - dl.origin[2];
        let dist = (dx * dx + dy * dy + dz * dz).sqrt();
        let add = dl.radius - dist;
        if add > 0.0 {
            ambient += add; // C: `lighting.ambientlight += add`
        }
    }
    // C clamps ambient to 128 (so it never fully whites out) before the lighting
    // table; here we normalise to a 0..~1.2 brightness. A pitch-black sample
    // (`ambient` near 0) still leaves the model dimly visible (floor ~0.25), and a
    // fully-lit/dlit sample saturates near 1.2 (a little overbright for dlights).
    let ambient = ambient.min(255.0);
    let model_light = (0.25 + ambient / 200.0).clamp(0.25, 1.2);

    // Model orientation (r_alias.c R_AliasSetUpTransform). With pitch=roll=0 this is
    // exactly the +Z yaw rotation, so zero-orientation models take the original fast
    // path and render bit-identically. With a pitch or roll (flying projectiles,
    // banking flyers) we build the full basis from AngleVectors and transform each
    // point as `p[0]*forward - p[1]*right + p[2]*up + origin` (the C t2matrix whose
    // columns are forward / -right / up over angles [PITCH=-pitch, YAW, ROLL]).
    let yaw_rad = (inst.yaw as f64).to_radians();
    let oriented = inst.pitch != 0.0 || inst.roll != 0.0;
    let (m_fwd, m_right, m_up) = if oriented {
        crate::math::angle_vectors([-inst.pitch, inst.yaw, inst.roll])
    } else {
        ([0.0; 3], [0.0; 3], [0.0; 3]) // unused on the fast path
    };

    let verts = match mdl_frame_verts(inst.mdl, inst.frame, time) {
        Some(v) => v,
        None => return, // no frame -> nothing to draw
    };
    let header = &inst.mdl.header;

    // Resolve the model's skin once, by the entity's skinnum and the scene time
    // (group skins animate). `None` => the whole model uses the flat colour path
    // (items without skins, malformed dims, short pixel buffers).
    let skin = mdl_skin(inst.mdl, inst.skinnum, time);

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
                *w = if oriented {
                    [
                        p[0] * m_fwd[0] - p[1] * m_right[0] + p[2] * m_up[0] + inst.origin[0],
                        p[0] * m_fwd[1] - p[1] * m_right[1] + p[2] * m_up[1] + inst.origin[1],
                        p[0] * m_fwd[2] - p[1] * m_right[2] + p[2] * m_up[2] + inst.origin[2],
                    ]
                } else {
                    mdl_model_to_world(p, yaw_rad, inst.origin)
                };
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
        // Brightness = world-sampled model light, modulated by a gentle Lambert so
        // the silhouette still reads (the C's `r_shadelight*lightcos` term). The
        // directional term only varies brightness within [0.7, 1.0]*model_light,
        // so the model never goes black on a back face — the world light dominates.
        let (light_dir, _l) = normalize([0.3, 0.5, 1.0]);
        let lambert = 0.7 + 0.3 * dot(normal, light_dir).max(0.0);
        let shade = (model_light * lambert).clamp(0.0, MAX_LIGHT_FACTOR);
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

        // FIX-3: screen-space backface cull, matching the WinQuake SOFTWARE
        // renderer. D_DrawNonSubdiv/D_DrawSubdiv (d_polyse.c:203,265) skip an
        // alias triangle whose final screen verts give `d_xdenom >= 0`, drawing
        // only front faces (`d_xdenom < 0`). Our `edge(v0,v1,v2)` signed area is
        // exactly `-d_xdenom` (verified algebraically), so a front face has
        // `area > 0` and we cull `area <= 0`. Verified visually: with this sign
        // a live grunt still fully renders (its back faces were already
        // z-occluded, so 0 visible pixels change); the opposite sign erases the
        // monster's front faces. The barycentric rasteriser still draws either
        // winding, so this cull only suppresses the now-redundant back faces.
        {
            let area = edge(xy[0].0, xy[0].1, xy[1].0, xy[1].1, xy[2].0, xy[2].1);
            if area <= 0.0 {
                continue;
            }
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
                    // Alias models are not colormapped (they keep the linear
                    // shade multiply, matching the C's separate alias path).
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
    /// The brush entity's `frame` field. func_button / func_door / func_plat set
    /// `self.frame = 1` when activated; a non-zero frame selects the `+a..+j`
    /// ALTERNATE animated-texture cycle (`R_TextureAnimation`), so e.g. a pressed
    /// button shows its lit/green face. 0 = the primary `+0..+9` cycle.
    pub frame: i32,
}

/// One *external* brush model placed in the world: an entire standalone BSP
/// (e.g. `maps/b_explob.bsp`, `maps/b_shell0.bsp`) drawn as a single item,
/// placed at `origin`.
///
/// In Quake many pickup/item entities (`misc_explobox`, `item_health`,
/// `item_shells`, `item_rockets`, `item_spikes`, `item_cells`, the armor) are
/// not alias models — their QuakeC `precache_model`/`setmodel` points at a tiny
/// brush BSP that ships in the pak (`maps/b_*.bsp`). Each such BSP is a complete
/// `version 29` map whose **MODEL 0** is the little box brush, defined around the
/// bsp's own local origin. The item entity then stands that box at its world
/// `origin`.
///
/// This differs from [`BModelInstance`], which references an *inline* submodel of
/// the **world** bsp by index. An `ExternalBModel` borrows a *separate*, already
/// parsed [`Bsp`] (so one cached parse can back many instances without cloning)
/// and always renders that bsp's model-0 faces. It is drawn by [`draw_brush_bsp`]
/// / [`render_scene_ext`], sharing the world z-buffer so the box occludes — and
/// is occluded by — the world and every other model correctly.
pub struct ExternalBModel<'a> {
    /// The parsed standalone brush BSP (its MODEL 0 is the visible box).
    pub bsp: &'a Bsp,
    /// World position to stand the box at (the item entity's `origin`).
    pub origin: Vec3,
}

/// Draw an **external** brush model's MODEL-0 faces into `image`, translated to
/// `origin` and z-tested against the shared `zbuf` — the render path for Quake's
/// `b_*.bsp` item boxes (explosive box, ammo/health boxes; see [`ExternalBModel`]).
///
/// This is the standalone-bsp counterpart to the world-submodel path. It reuses
/// the exact same brush-face machinery as [`draw_submodel`] (texinfo (s,t) build,
/// near-clip via [`clip_poly_near`], perspective projection, fan rasterise with
/// the bsp's **own** miptextures, the static/multi-style lightmap via
/// [`face_lightmap_dyn`], and the per-pixel z-test) — only the source bsp differs,
/// so there is no duplicated rasteriser. Because these little boxes carry their
/// own textures and a baked lightmap (and most are effectively fullbright), a
/// face with a lightmap uses it and a face without one renders fullbright, exactly
/// as [`draw_submodel`] already handles.
///
/// MODEL index 0 (the whole box brush) is always the one drawn; the function is a
/// thin wrapper over the shared [`draw_submodel`] implementation with
/// `model_index = 0`.
///
/// SAFETY: every bsp-array access in the shared path goes through `.get()`, so a
/// malformed or empty external bsp (no models, an out-of-range face/edge/plane/
/// texinfo) simply draws nothing rather than panicking.
#[allow(clippy::too_many_arguments)]
pub fn draw_brush_bsp(
    image: &mut Image,
    zbuf: &mut [f32],
    cam: &Camera,
    bsp: &Bsp,
    origin: Vec3,
    palette: &[[u8; 3]; 256],
    time: f32,
    light_styles: &[f32; LIGHTSTYLES],
) {
    // The little box bsps are not dynamically lit in the original game; pass no
    // dynamic lights (so the static/multi-style lightmap, or fullbright, is used).
    // A fresh turbulent SIN table is built per call — these boxes have no liquid
    // surfaces in practice, but the shared path needs a table to satisfy the type;
    // it is cheap (a 256-entry `[f32]`, no global state). Callers that draw many
    // boxes per frame go through `render_scene_ext`, which builds ONE table and
    // calls the shared `draw_submodel` directly, so this per-call table only costs
    // when `draw_brush_bsp` is used standalone (e.g. tests).
    let turb = TurbTable::new();
    draw_submodel(
        image,
        zbuf,
        bsp,
        cam,
        palette,
        0,
        origin,
        &turb,
        time,
        light_styles,
        &[],
        // Standalone box draw keeps the legacy linear shade (no colormap),
        // byte-identical to before; the colormap is a render_scene_ext concern.
        None,
        0,     // ent_frame: standalone box has no activated/alternate state
        false, // external box bsp -> bypass the surface cache (no colormap anyway)
    );
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

/// A sprite-model entity to draw as a camera-facing billboard (Quake's
/// `mod_sprite` entities: the `s_explod.spr` explosion flash, bubbles, etc.).
pub struct SpriteInstance<'a> {
    pub sprite: &'a crate::spr::Sprite,
    /// World position of the sprite centre (the entity origin).
    pub origin: Vec3,
    /// Top-level frame index (clamped); a group frame animates by `time`.
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
    // The view-space offset (in MDL/world units) that hangs the gun at the eye,
    // slightly right of and below centre, matching Quake's hand-held pose. These
    // are added in the camera basis below (forward / right / up). Quake draws the
    // viewmodel essentially AT the eye and lets the near plane CLIP the grip: the
    // `v_*` weapon models span roughly model-X (forward) in [-14, +28] and
    // model-Z (up) in [-12, 0] (below the eye). With the near-plane CLIPPING now
    // in place (`clip_poly_near` below), the grip (model-X < 0, behind the eye)
    // is trimmed at the plane while the barrel (0..28) extends forward — so the
    // gun sits large at the lower-centre/right of the frame instead of being
    // shoved 16..58 units ahead (the old +30 push made it look distant + tiny).
    //
    // `OFS_FORWARD` is a tiny positive nudge: it only keeps the grip from landing
    // exactly on the near plane (a degenerate edge) — it does NOT push the gun
    // away. `OFS_RIGHT` (negative -> camera right, since the gun is anchored with
    // `-p[1]` on `right`) nudges it just right of centre; `OFS_UP` lifts the
    // already-low (model-Z < 0) barrel up so the grip is clipped at the bottom
    // edge rather than the whole gun falling off-screen.
    //
    // The placement is in *proportion* resolution-independent: focal length
    // scales with the frame width and the screen centre with its size, so the
    // gun keeps the same lower-centre fraction of the frame at any `w`/`h`.
    const OFS_FORWARD: f32 = 7.0;
    const OFS_RIGHT: f32 = 1.5;
    const OFS_UP: f32 = 3.5;
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

    // The viewmodel carries only a `frame` (no group-anim time / skinnum), so it
    // poses at `time = 0` (first sub-pose of any group) with skin 0 — its prior
    // behaviour. An out-of-range frame still resets to 0 inside `mdl_frame_verts`.
    let verts = match mdl_frame_verts(mdl, frame, 0.0) {
        Some(v) => v,
        None => return, // no frame -> nothing to draw
    };
    let header = &mdl.header;
    let skin = mdl_skin(mdl, 0, 0.0);

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

        // Build the three view-space verts (vx/vy/vz on the right/up/forward
        // axes) carrying the per-vertex skin (s, t) already computed above, then
        // CLIP against the near plane instead of dropping the whole triangle the
        // moment one vertex falls at/behind it. This is the same Sutherland–
        // Hodgman path the world / submodel passes use (`clip_poly_near`), so the
        // grip (model-X < 0, behind the eye) is trimmed at the plane while the
        // barrel ahead of the eye still draws — the authentic held-gun pose.
        let in_st = st.unwrap_or([(0.0, 0.0); 3]);
        let mut vviews: [VView; 3] = [VView { vx: 0.0, vy: 0.0, vz: 0.0, s: 0.0, t: 0.0 }; 3];
        for (slot, v) in world.iter().enumerate() {
            let rel = sub(*v, cam.pos);
            if let (Some(slot_v), Some(&(s, t))) = (vviews.get_mut(slot), in_st.get(slot)) {
                *slot_v = VView {
                    vx: dot(rel, right),
                    vy: dot(rel, up),
                    vz: dot(rel, forward),
                    s,
                    t,
                };
            }
        }
        let _ = NEAR; // the plane lives in `clip_poly_near` (`NEAR_PLANE`, == NEAR)
        let poly = clip_poly_near(&vviews);
        if poly.len() < 3 {
            continue; // wholly behind the eye -> nothing to draw
        }

        // Project the clipped polygon to screen (every `vz >= NEAR` now).
        let proj: Vec<ProjT> = poly
            .iter()
            .map(|v| ProjT {
                x: cx + focal * v.vx / v.vz,
                y: cy - focal * v.vy / v.vz,
                vz: v.vz,
                s: v.s,
                t: v.t,
            })
            .collect();

        // FIX-3: screen-space backface cull on the PROJECTED polygon, matching
        // the WinQuake SOFTWARE renderer (D_DrawNonSubdiv/D_DrawSubdiv reject a
        // triangle whose final screen verts give `d_xdenom >= 0`). Our
        // `edge(v0,v1,v2)` signed area is exactly `-d_xdenom`, so a front face
        // has `area > 0`; we cull `area <= 0`. Using the first three clipped
        // verts is the same winding test as the unclipped path (clipping is a
        // convex truncation, so it preserves orientation).
        {
            let area = edge(
                proj[0].x, proj[0].y, proj[1].x, proj[1].y, proj[2].x, proj[2].y,
            );
            if area <= 0.0 {
                continue;
            }
        }

        // Fan-rasterise the clipped polygon (verts 0, i, i+1) into the PRIVATE
        // z-buffer. `(s, t)` interpolate correctly through `clip_poly_near` +
        // the perspective-correct raster, exactly as the world path; the flat
        // fallback (no usable skin) draws the shaded grey through the same fan.
        for i in 1..proj.len() - 1 {
            let (v0, v1, v2) = (proj[0], proj[i], proj[i + 1]);
            match &skin {
                Some(sk) if st.is_some() => {
                    raster_triangle_tex(
                        image, &mut local_z, v0, v1, v2,
                        sk.pixels, sk.width, sk.height, palette, shade, None, SurfaceMode::Normal,
                        // Viewmodel is not colormapped (linear shade multiply).
                        None,
                    );
                }
                _ => {
                    let p = |v: &ProjT| Projected { x: v.x, y: v.y, depth: v.vz };
                    raster_triangle(image, &mut local_z, p(&v0), p(&v1), p(&v2), flat);
                }
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
        &[],
        None,
        0.0,
        &[],
        &[],
        &NEUTRAL_LIGHTSTYLE_SCALES,
        None,
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
/// `bmodels`/`external` slice and `None` `viewmodel` reproduces [`render_scene`]
/// exactly.
///
/// ## External brush models (item boxes)
/// `external` is the set of standalone `b_*.bsp` item boxes — Quake's
/// `misc_explobox` and the ammo/health pickup boxes (see [`ExternalBModel`]).
/// Each entry borrows its own parsed [`Bsp`] and is drawn (MODEL 0, translated to
/// the item origin) by the same brush-face path as the world submodels, **after**
/// the world and inline submodels but before the alias models, sharing the one
/// z-buffer so the box occludes / is occluded correctly. These boxes are not
/// dynamically lit in the original game, so they take the static (or fullbright)
/// lightmap. An empty `external` slice draws nothing — byte-identical to the
/// pre-external renderer, which is why every prior caller passes `&[]`.
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
///
/// ## Colormap (exact Quake shading)
/// `colormap` is Quake's `gfx/colormap.lmp`: `64 * 256` bytes — 64 light rows of
/// 256 palette indices each, row 0 brightest, row 63 darkest. When `Some`, a lit
/// wall pixel is shaded the way the software renderer does: the per-pixel
/// lightmap brightness selects a colormap ROW (`colormap_row`, ported from
/// `R_BuildLightMap`'s bound/invert/shift), `colormap[row*256 + texel]` yields a
/// PALETTE INDEX, and the final colour is `palette[that index]` — an index
/// lookup that bakes Quake's non-linear darkening and CANNOT overbright past the
/// base colour. Liquids and sky stay fullbright at row 0 (`colormap[texel]`).
/// When `None`, the renderer keeps the legacy linear `palette[texel] *
/// brightness` multiply byte-for-byte, so [`render_scene`] and every existing
/// caller/test are unchanged. A colormap shorter than `64*256` bytes is ignored
/// (treated as `None`) rather than read out of bounds.
#[allow(clippy::too_many_arguments)]
pub fn render_scene_ext(
    bsp: &Bsp,
    cam: &Camera,
    w: usize,
    h: usize,
    palette: &[[u8; 3]; 256],
    models: &[ModelInstance],
    bmodels: &[BModelInstance],
    external: &[ExternalBModel],
    viewmodel: Option<Viewmodel>,
    time: f32,
    particles: &[(Vec3, u8)],
    dlights: &[crate::dlight::DynamicLight],
    light_styles: &[f32; LIGHTSTYLES],
    colormap: Option<&[u8]>,
) -> Image {
    // The 14-arg entry point every test / tool caller uses: no sprite entities.
    render_scene_ext_sprited(
        bsp, cam, w, h, palette, models, bmodels, external, viewmodel, time, particles, dlights,
        light_styles, colormap, &[],
    )
}

/// As [`render_scene_ext`], plus a list of camera-facing [`SpriteInstance`]s drawn
/// (z-tested) after the alias models and before the viewmodel — Quake's
/// `mod_sprite` entities (the `s_explod.spr` explosion flash, bubbles). Passing an
/// empty `sprites` slice is byte-identical to [`render_scene_ext`].
#[allow(clippy::too_many_arguments)]
pub fn render_scene_ext_sprited(
    bsp: &Bsp,
    cam: &Camera,
    w: usize,
    h: usize,
    palette: &[[u8; 3]; 256],
    models: &[ModelInstance],
    bmodels: &[BModelInstance],
    external: &[ExternalBModel],
    viewmodel: Option<Viewmodel>,
    time: f32,
    particles: &[(Vec3, u8)],
    dlights: &[crate::dlight::DynamicLight],
    light_styles: &[f32; LIGHTSTYLES],
    colormap: Option<&[u8]>,
    sprites: &[SpriteInstance],
) -> Image {
    let mut image = Image::new(w, h, [10, 10, 14]);
    if w == 0 || h == 0 {
        return image;
    }
    let mut zbuf = vec![f32::INFINITY; w.saturating_mul(h)];
    // The turbulent SIN table for liquid warp, built once and shared by the
    // world + brush-submodel passes (sky needs no table).
    let turb = TurbTable::new();
    // Phase wall-timers: `Instant::now()` is only evaluated when the profiler is on
    // (via `.then(..)`), so the shared render path — and wasm, where the profiler is
    // never enabled and `Instant` is unavailable — never constructs one.
    let tw = stats_on().then(std::time::Instant::now);
    draw_world_textured(&mut image, &mut zbuf, bsp, cam, palette, &turb, time, light_styles, dlights, colormap);
    if let Some(t) = tw { stat(|s| s.world_ns += t.elapsed().as_nanos() as u64); }
    let ts = stats_on().then(std::time::Instant::now);
    for bm in bmodels {
        // Inline submodels share the world `bsp`, so their surface blocks ARE cached.
        draw_submodel(&mut image, &mut zbuf, bsp, cam, palette, bm.model_index, bm.origin, &turb, time, light_styles, dlights, colormap, bm.frame, true);
    }
    if let Some(t) = ts { stat(|s| s.submodel_ns += t.elapsed().as_nanos() as u64); }
    let te = stats_on().then(std::time::Instant::now);
    // External brush models (Quake's `b_*.bsp` item boxes: explosive box, ammo
    // and health boxes). Each draws its OWN bsp's MODEL-0 faces, translated to the
    // item origin, against the shared z-buffer so it occludes/ is occluded by the
    // world correctly. These boxes are not dynamically lit in the original game,
    // so the shared submodel path is called with no dlights (its static/multi-
    // style lightmap, or fullbright, is used). The one `turb` table built above is
    // reused, so drawing N boxes builds no extra tables. An empty `external` slice
    // draws nothing, leaving the image identical to the pre-external behaviour —
    // which is why `render_scene` and every prior caller can pass `&[]`.
    for ext in external {
        // External item boxes re-clone their bsp per instance every frame, so they
        // BYPASS the surface cache (cache_surf = false) — caching them would only
        // evict the world's resident cache. See [`face_surf_block`].
        draw_submodel(&mut image, &mut zbuf, ext.bsp, cam, palette, 0, ext.origin, &turb, time, light_styles, &[], colormap, 0, false);
    }
    if let Some(t) = te { stat(|s| s.external_ns += t.elapsed().as_nanos() as u64); }
    let ta = stats_on().then(std::time::Instant::now);
    for inst in models {
        draw_alias_model(&mut image, &mut zbuf, bsp, cam, inst, w, h, palette, dlights, light_styles, time);
    }
    if let Some(t) = ta { stat(|s| s.alias_ns += t.elapsed().as_nanos() as u64); }
    // Particles draw after the world/models, z-tested against the same buffer so
    // walls occlude them, but before the viewmodel (which always draws on top).
    let tp = stats_on().then(std::time::Instant::now);
    draw_particles(&mut image, &mut zbuf, cam, particles, palette, w, h);
    if let Some(t) = tp { stat(|s| s.particle_ns += t.elapsed().as_nanos() as u64); }
    // Sprite-model entities (explosion flash, bubbles) — camera-facing billboards,
    // z-tested against the same buffer, drawn after models and before the viewmodel.
    let tsp = stats_on().then(std::time::Instant::now);
    draw_sprites(&mut image, &mut zbuf, cam, sprites, palette, time, w, h);
    if let Some(t) = tsp { stat(|s| s.sprite_ns += t.elapsed().as_nanos() as u64); }
    // The weapon viewmodel draws last, on top of the world and every model.
    let tv = stats_on().then(std::time::Instant::now);
    if let Some(vm) = viewmodel {
        draw_viewmodel(&mut image, &mut zbuf, cam, vm.mdl, vm.frame, palette, w, h);
    }
    if let Some(t) = tv { stat(|s| s.viewmodel_ns += t.elapsed().as_nanos() as u64); }
    image
}

/// Draw camera-facing sprite-model entities (Quake `mod_sprite`), z-tested against
/// the shared `zbuf`. Each sprite's active frame (a [`crate::spr::Frame::Single`]
/// or a `Group` whose sub-frame is selected by `time`) is rasterised as a
/// screen-aligned billboard: the sprite faces the camera, so a frame W×H pixels (1
/// texel = 1 world unit) spans `focal*W/vz` × `focal*H/vz` framebuffer pixels around
/// the projected origin, offset by the frame's `origin` (left/up). Palette index 255
/// is transparent (`d_sprite.c`). Nearest-neighbour sampled; behind-wall pixels are
/// hidden by the depth test (`vz < zbuf`) and write depth so nearer geometry wins.
/// Oriented sprites fall back to the facing billboard (good enough for shareware).
// Mirrors R_DrawSprite (r_sprite.c); the C reads globals (vid, r_refdef, cl.time)
// that this port passes explicitly.
#[allow(clippy::too_many_arguments)]
fn draw_sprites(
    image: &mut Image,
    zbuf: &mut [f32],
    cam: &Camera,
    sprites: &[SpriteInstance],
    palette: &[[u8; 3]; 256],
    time: f32,
    w: usize,
    h: usize,
) {
    const NEAR: f32 = 1.0;
    if w == 0 || h == 0 || sprites.is_empty() {
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

    for inst in sprites {
        let Some(frame) = select_sprite_frame(inst.sprite, inst.frame, time) else {
            continue;
        };
        if frame.width <= 0 || frame.height <= 0 {
            continue;
        }
        let rel = sub(inst.origin, cam.pos);
        let vz = dot(rel, forward);
        if vz <= NEAR {
            continue; // at/behind the near plane
        }
        let sx = cx + focal * dot(rel, right) / vz;
        let sy = cy - focal * dot(rel, up) / vz;
        // 1 texel = 1 world unit; the facing billboard scales by focal/vz. The frame
        // `origin` is the left/up offset of its top-left from the centre (Quake:
        // up = origin[1], down = origin[1]-height, left = origin[0]).
        let scale = focal / vz;
        let (fw, fh) = (frame.width as f32, frame.height as f32);
        let (ox, oy) = (frame.origin[0] as f32, frame.origin[1] as f32);
        // +up is -screen-y; the top edge is at world up-offset `oy`.
        let x0 = sx + ox * scale;
        let x1 = sx + (ox + fw) * scale;
        let y0 = sy - oy * scale;
        let y1 = sy - (oy - fh) * scale;
        let (px0, px1) = (x0.min(x1), x0.max(x1));
        let (py0, py1) = (y0.min(y1), y0.max(y1));
        if !(px0.is_finite() && px1.is_finite() && py0.is_finite() && py1.is_finite()) {
            continue;
        }
        let ix0 = px0.floor().max(0.0) as usize;
        let ix1 = (px1.ceil() as i64).clamp(0, w as i64) as usize;
        let iy0 = py0.floor().max(0.0) as usize;
        let iy1 = (py1.ceil() as i64).clamp(0, h as i64) as usize;
        let span_x = (px1 - px0).max(1e-6);
        let span_y = (py1 - py0).max(1e-6);
        let (tw, th) = (frame.width as usize, frame.height as usize);
        for py in iy0..iy1 {
            // Texel row: fraction down the screen rect -> 0..th-1.
            let tv = (((py as f32 + 0.5 - py0) / span_y) * th as f32) as usize;
            let tv = tv.min(th - 1);
            for px in ix0..ix1 {
                let tu = (((px as f32 + 0.5 - px0) / span_x) * tw as f32) as usize;
                let tu = tu.min(tw - 1);
                let texel = frame.pixels[tv * tw + tu];
                if texel == 255 {
                    continue; // transparent
                }
                let idx = py * w + px;
                if vz < zbuf[idx] {
                    image.rgb[idx] = palette[texel as usize];
                    zbuf[idx] = vz;
                }
            }
        }
    }
}

/// Select a sprite's active [`crate::spr::SpriteFrame`] for top-level `frame` index
/// and game `time`, porting `R_GetSpriteframe`: an out-of-range index clamps to 0; a
/// `Group` picks the sub-frame by `targettime = time mod intervals[last]` (first
/// interval strictly greater than targettime).
fn select_sprite_frame(
    sprite: &crate::spr::Sprite,
    frame: usize,
    time: f32,
) -> Option<&crate::spr::SpriteFrame> {
    let f = sprite.frames.get(frame).or_else(|| sprite.frames.first())?;
    match f {
        crate::spr::Frame::Single(sf) => Some(sf),
        crate::spr::Frame::Group { intervals, frames } => {
            let full = intervals.last().copied().unwrap_or(0.0);
            let i = if full > 0.0 && full.is_finite() {
                let targettime = time - (time / full).floor() * full;
                intervals.iter().position(|&iv| iv > targettime).unwrap_or(0)
            } else {
                0
            };
            frames.get(i).or_else(|| frames.first())
        }
    }
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
/// A particle is drawn as a `pix`x`pix` filled square whose side scales
/// **continuously** with `1/z`, porting `R_DrawParticles`/`D_DrawParticle`
/// (`d_part.c`): the C computes `izi = zi*0x8000` (`zi = 1/z`),
/// `pix = izi >> d_pix_shift`, then clamps to `[d_pix_min, d_pix_max]`. The
/// resolution-derived constants are `d_pix_min = max(1, width/320)`,
/// `d_pix_max = round(width/80)`, `d_pix_shift = 8 - round(width/320)`
/// (`d_modech.c`). We reproduce that same continuous ramp (rather than a
/// 2-bucket step) so a particle grows smoothly as it nears the eye and shrinks to
/// the minimum size far away. For every covered pixel the existing z-buffer test
/// is reused: the pixel is written only when `vz < zbuf[idx]` (strictly nearer),
/// and the depth is written so later, nearer geometry can still overdraw it.
/// Off-screen pixels are clipped by the loop bounds; the colour is
/// `palette[color]`.
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
        let sy = cy - focal * vy / vz;
        if !(sx.is_finite() && sy.is_finite()) {
            continue;
        }

        // Continuous 1/z size ramp (D_DrawParticle): the projected on-screen size
        // of a ~1-unit particle is `focal/vz`; this grows smoothly as the particle
        // nears the eye. Clamp to the resolution-scaled `[d_pix_min, d_pix_max]`.
        let pix = (focal / vz).round() as i64;
        let pix = pix.clamp(d_pix_min, d_pix_max);

        let rgb = palette[color as usize];

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
    // One call appends to all five BSP lump buffers (vertex/edge/surfedge/face/plane).
    #[allow(clippy::too_many_arguments)]
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

// ---------------------------------------------------------------------------
// Screen layout: scr_viewsize -> the 3-D view rectangle + sb_lines
// (SCR_CalcRefdef / R_SetVrect / Draw_TileClear)
// ---------------------------------------------------------------------------

/// `scr_viewsize` ("viewsize", screen.c: default "100", archived) and its
/// bounds: SCR_CalcRefdef clamps it to 30..=120 and `M_AdjustSliders` /
/// `sizeup` / `sizedown` move it in steps of 10. 100 is the full-width view
/// above the full status bar; 110 drops the inventory strip; 120 drops the
/// status bar entirely; below 100 the view shrinks, centred, inside a
/// `backtile` border.
pub const VIEWSIZE_DEFAULT: f32 = 100.0;
pub const VIEWSIZE_MIN: f32 = 30.0;
pub const VIEWSIZE_MAX: f32 = 120.0;
pub const VIEWSIZE_STEP: f32 = 10.0;

/// `sb_lines` for the full status bar: the 24-row `sbar` plus the 24-row
/// `ibar` inventory strip (SCR_CalcRefdef's `24+16+8`).
pub const SB_LINES_FULL: i32 = 48;

/// A rectangle of the framebuffer, in pixels (`vrect_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewRect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

/// What `SCR_CalcRefdef` works out each time the view changes: where the 3-D
/// view goes (`r_refdef.vrect`) and how many status-bar lines are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refdef {
    /// `r_refdef.vrect`: the 3-D view rectangle in framebuffer pixels. The
    /// renderer draws into a `vrect.w x vrect.h` image with the projection
    /// centred on it and `fov_x` spanning its width — R_ViewChanged's
    /// `xcenter = vrect.width/2 + vrect.x`, `xscale = vrect.width / (2 tan(fov_x/2))`,
    /// `yscale = xscale` (square pixels); the software renderer derives its
    /// vertical extent from that, not from CalcFov's `fov_y`.
    pub vrect: ViewRect,
    /// `sb_lines` in the status bar's 320x200 virtual rows: 48 (sbar + inventory),
    /// 24 (sbar only) or 0 (no status bar).
    pub sb_lines: i32,
}

/// `SCR_CalcRefdef` + `R_SetVrect` (screen.c / r_main.c) for a `vid_w x vid_h`
/// framebuffer: bound `viewsize` to 30..=120, pick `sb_lines` (an intermission
/// is always full screen: `size = 120`), then size the view rectangle —
/// `viewsize`% of the screen (100 at most), at least 96 wide, width a multiple
/// of 8 and height even, never taller than the screen minus the status bar,
/// centred horizontally on the screen and vertically in the space above the
/// status bar.
///
/// The one adaptation: this port draws the 2-D layer (status bar, menus) as the
/// 320x200 virtual screen scaled by `vid_w/320`, so the status bar the view must
/// clear (`lineadj`) is `sb_lines` scaled to framebuffer rows — exactly the
/// rows [`draw_hud_into`] paints. At 320x200 every number is the C's.
///
/// The arithmetic keeps the C's types: `size` is a `float`, the products are
/// truncated to `int` (so e.g. 70% of 320 is `(int)(320 * 0.7f) = 224`, as an
/// IEEE-single build computes it).
pub fn calc_refdef(vid_w: usize, vid_h: usize, viewsize: f32, intermission: bool) -> Refdef {
    // SCR_CalcRefdef: bound viewsize (a non-number reads as the default).
    let viewsize = if viewsize.is_finite() {
        viewsize.clamp(VIEWSIZE_MIN, VIEWSIZE_MAX)
    } else {
        VIEWSIZE_DEFAULT
    };
    // "intermission is always full screen"
    let size = if intermission { 120.0 } else { viewsize };
    let sb_lines = if size >= 120.0 {
        0 // no status bar at all
    } else if size >= 110.0 {
        24 // no inventory
    } else {
        SB_LINES_FULL
    };
    // The status bar's framebuffer rows: draw_hud_into scales the 320-wide bar
    // by vid_w/320 and bottom-anchors it, so it covers ceil(sb_lines * scale).
    let vw = vid_w as i64;
    let vh = vid_h as i64;
    let scale = vid_w as f32 / HUD_VIRT_W;
    let mut lineadj = ((sb_lines as f32 * scale).ceil() as i64).clamp(0, vh);

    // R_SetVrect (r_main.c).
    let mut size: f32 = if viewsize > 100.0 { 100.0 } else { viewsize };
    if intermission {
        size = 100.0;
        lineadj = 0;
    }
    size /= 100.0;
    let h = vh - lineadj;
    let mut w = (vw as f32 * size) as i64;
    if w < 96 {
        size = (96.0 / vw.max(1) as f64) as f32;
        w = 96; // min for icons
    }
    w &= !7;
    let mut height = (vh as f32 * size) as i64;
    if height > vh - lineadj {
        height = vh - lineadj;
    }
    height &= !1;
    // (A frame narrower than 96/8 px never occurs in the C; clamp so a tiny
    // test framebuffer still yields an in-bounds rectangle.)
    let w = w.clamp(0, vw);
    let height = height.clamp(0, vh);
    let x = ((vw - w) / 2).max(0);
    let y = ((h - height) / 2).max(0);
    Refdef {
        vrect: ViewRect { x: x as usize, y: y as usize, w: w as usize, h: height as usize },
        sb_lines,
    }
}

/// `Draw_TileClear` (draw.c): fill the framebuffer rectangle `(x, y, w, h)` with
/// the 64x64 `backtile` pic, tiled from the SCREEN origin (texel
/// `(x mod 64, y mod 64)`), as SCR_UpdateScreen does under a view smaller than
/// the screen. Like the rest of the 2-D layer the tile is the 320x200 virtual
/// screen's, scaled by `vid_w/320` (nearest-neighbour). A missing or malformed
/// `backtile` fills black; every write is clipped.
pub fn draw_tile_clear(
    image: &mut Image,
    backtile: Option<&crate::wad::Qpic>,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    palette: &[[u8; 3]; 256],
) {
    let x1 = x.saturating_add(w).min(image.w);
    let y1 = y.saturating_add(h).min(image.h);
    if x >= x1 || y >= y1 {
        return;
    }
    let tile = backtile.filter(|t| {
        t.width > 0 && t.height > 0 && t.data.len() >= (t.width as usize) * (t.height as usize)
    });
    let scale = image.w as f32 / HUD_VIRT_W;
    let inv = if scale.is_finite() && scale > 0.0 { 1.0 / scale } else { 1.0 };
    for py in y..y1 {
        let row = &mut image.rgb[py * image.w..py * image.w + image.w];
        let Some(t) = tile else {
            row[x..x1].fill([0, 0, 0]);
            continue;
        };
        let (tw, th) = (t.width as usize, t.height as usize);
        let ty = ((py as f32 * inv) as usize) % th;
        let trow = &t.data[ty * tw..ty * tw + tw];
        for (px, out) in row.iter_mut().enumerate().take(x1).skip(x) {
            let tx = ((px as f32 * inv) as usize) % tw;
            *out = palette[trow[tx] as usize];
        }
    }
}

/// Put the rendered 3-D `view` (a `vrect.w x vrect.h` image) into a
/// `vid_w x vid_h` screen at `vrect`, with everything outside it tile-cleared
/// ([`draw_tile_clear`] — SCR_UpdateScreen's `Draw_TileClear(0,0,vid.width,
/// vid.height)` under the view). A view that already IS the whole screen
/// (viewsize 120, an intermission) comes back untouched, at zero cost. The
/// status bar is drawn over the result afterwards, as in the C.
pub fn compose_view(
    view: Image,
    vrect: ViewRect,
    vid_w: usize,
    vid_h: usize,
    backtile: Option<&crate::wad::Qpic>,
    palette: &[[u8; 3]; 256],
) -> Image {
    if vrect.x == 0 && vrect.y == 0 && view.w == vid_w && view.h == vid_h {
        return view;
    }
    let mut img = Image::new(vid_w, vid_h, [0, 0, 0]);
    draw_tile_clear(&mut img, backtile, 0, 0, vid_w, vid_h, palette);
    let cw = view.w.min(vid_w.saturating_sub(vrect.x));
    for vy in 0..view.h {
        let py = vrect.y + vy;
        if py >= vid_h {
            break;
        }
        let dst = py * vid_w + vrect.x;
        img.rgb[dst..dst + cw].copy_from_slice(&view.rgb[vy * view.w..vy * view.w + cw]);
    }
    img
}

/// The Quake HUD overlay: the parsed `gfx.wad`, the screen palette, and the
/// player stats to display. Built by the caller each frame from the player edict
/// and the loaded `gfx.wad`; consumed by [`draw_hud_into`].
///
/// The `wad`/`palette` borrows carry an explicit lifetime `'a` so the caller can
/// keep one parsed [`Wad2`] alive and lend it per frame without cloning.
pub struct Hud<'a> {
    /// The parsed `gfx.wad`, which holds the `sbar`/`ibar`/`num_*`/`anum_*`/face/
    /// weapon/item/ammo/armor pics and the `conchars` font.
    pub wad: &'a crate::wad::Wad2,
    /// The screen palette (`gfx/palette.lmp`), used to colour the pic texels.
    pub palette: &'a [[u8; 3]; 256],
    /// Current player health, drawn as a big number on the left of the bar, and
    /// driving the face-frame bracket in [`Sbar_DrawFace`](draw_hud_into).
    pub health: i32,
    /// Current ammo for the active weapon, drawn on the right of the bar.
    pub ammo: i32,
    /// Current armour value, drawn just right of the armour icon.
    pub armor: i32,
    /// The QuakeC `items` bitfield (`cl.items`): weapons (`IT_SHOTGUN`..
    /// `IT_LIGHTNING`), ammo-type bits (`IT_SHELLS`..`IT_CELLS`), armour type
    /// (`IT_ARMOR1/2/3`), keys (`IT_KEY1/2`), powerups (`IT_INVISIBILITY`,
    /// `IT_INVULNERABILITY`, `IT_SUIT`, `IT_QUAD`) and sigils (`IT_SIGIL1..4`).
    pub items: i32,
    /// The active weapon's `IT_*` bit (QuakeC `weapon` / `cl.stats[STAT_ACTIVEWEAPON]`):
    /// selects which inventory icon flashes and (via its ammo type) is highlighted.
    pub weapon: i32,
    /// Shell count (QuakeC `ammo_shells`), drawn small in the ibar's first slot.
    pub ammo_shells: i32,
    /// Nail count (QuakeC `ammo_nails`), second ibar ammo slot.
    pub ammo_nails: i32,
    /// Rocket count (QuakeC `ammo_rockets`), third ibar ammo slot.
    pub ammo_rockets: i32,
    /// Cell count (QuakeC `ammo_cells`), fourth ibar ammo slot.
    pub ammo_cells: i32,
    /// The server clock in seconds (`cl.time`), driving the selected-weapon flash
    /// cycle and the face pain/grimace animation. (The orchestrator passes the
    /// raw server time; the per-item acquire times of `cl.item_gettime[]` are not
    /// tracked here, so weapon icons show their static owned/selected frame — see
    /// the weapon-flash note in [`draw_hud_into`].)
    pub time: f32,
    /// Killed monsters / total (`cl.stats[STAT_MONSTERS/STAT_TOTALMONSTERS]`) for the
    /// solo scoreboard shown on death or Tab.
    pub monsters: i32,
    pub total_monsters: i32,
    /// Found secrets / total (`cl.stats[STAT_SECRETS/STAT_TOTALSECRETS]`).
    pub secrets: i32,
    pub total_secrets: i32,
    /// The level name (worldspawn `message`), right-justified on the scoreboard.
    pub level_name: &'a str,
    /// Force the scorebar + solo scoreboard (Tab "show scores"); the C also shows it
    /// whenever `cl.stats[STAT_HEALTH] <= 0`, which [`draw_hud_into`] handles directly.
    pub show_scores: bool,
    /// `sb_lines` from [`calc_refdef`] (the viewsize): 48 draws the inventory
    /// strip and the status bar, 24 the status bar alone, 0 neither — though
    /// the death / Tab scoreboard still shows at 0, as in `Sbar_Draw`.
    pub sb_lines: i32,
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

// ---------------------------------------------------------------------------
// Status-bar item bits (`quakedef.h` IT_* / the QuakeC `items` bitfield) and the
// gfx.wad lump-name tables (`Sbar_Init`). These drive Sbar_DrawInventory,
// Sbar_DrawFace and the armour/ammo-type icons.
// ---------------------------------------------------------------------------

/// `items` bit for owning the shotgun — the base of the 7 weapon bits. Weapon `i`
/// (0..6) is owned when `items & (IT_SHOTGUN << i)` is set, matching
/// `Sbar_DrawInventory`'s `cl.items & (IT_SHOTGUN<<i)` loop. The 7 bits in order
/// are shotgun(1), super-shotgun(2), nailgun(4), super-nailgun(8),
/// grenade-launcher(16), rocket-launcher(32), lightning(64).
const IT_SHOTGUN: i32 = 1;
const IT_SHELLS: i32 = 256;
const IT_NAILS: i32 = 512;
const IT_ROCKETS: i32 = 1024;
const IT_CELLS: i32 = 2048;
const IT_ARMOR1: i32 = 8192;
const IT_ARMOR2: i32 = 16384;
const IT_ARMOR3: i32 = 32768;
const IT_INVISIBILITY: i32 = 524288; // 1<<19
const IT_INVULNERABILITY: i32 = 1048576; // 1<<20
const IT_SUIT: i32 = 2097152; // 1<<21 (Biosuit)
const IT_QUAD: i32 = 4194304; // 1<<22

/// `inv_*` (owned, dim) weapon icon lump names, `sb_weapons[0][i]` in `Sbar_Init`.
const WEAPON_INV_NAMES: [&str; 7] = [
    "inv_shotgun", "inv_sshotgun", "inv_nailgun", "inv_snailgun", "inv_rlaunch", "inv_srlaunch",
    "inv_lightng",
];
/// The per-weapon name suffixes (`*_shotgun` … `*_lightng`) shared by the
/// `inv_*`/`inv2_*`/`inva{1..5}_*` icon families (`Sbar_Init`). Used to build the
/// selection-flash frame names for the active weapon.
const WEAPON_SUFFIX: [&str; 7] = [
    "shotgun", "sshotgun", "nailgun", "snailgun", "rlaunch", "srlaunch", "lightng",
];

/// `sb_ammo[type]` ammo-icon lump names (`Sbar_Init`): shells/nails/rocket/cells.
const AMMO_ICON_NAMES: [&str; 4] = ["sb_shells", "sb_nails", "sb_rocket", "sb_cells"];

/// `sb_armor[type]` armour-icon lump names (`Sbar_Init`).
const ARMOR_ICON_NAMES: [&str; 3] = ["sb_armor1", "sb_armor2", "sb_armor3"];

/// `sb_items[0..6]` (`Sbar_Init`): the keys + powerup icons drawn on the ibar.
/// In `items`-bit order from bit 17: key1, key2, invisibility(ring), invuln(pent),
/// suit, quad — matching `cl.items & (1<<(17+i))`.
const SB_ITEM_NAMES: [&str; 6] =
    ["sb_key1", "sb_key2", "sb_invis", "sb_invuln", "sb_suit", "sb_quad"];

/// `sb_sigil[0..3]` (`Sbar_Init`): the 4 runes, `cl.items & (1<<(28+i))`.
const SB_SIGIL_NAMES: [&str; 4] = ["sb_sigil1", "sb_sigil2", "sb_sigil3", "sb_sigil4"];

/// `sb_faces[f][0]` static-face lump names by health bracket, where bracket 0 is
/// the lowest health (`face5`) and bracket 4 (`face1`) the highest, mirroring
/// `Sbar_Init`'s `sb_faces[4]="face1" … sb_faces[0]="face5"`. Indexed `[bracket]`.
const FACE_NAMES: [&str; 5] = ["face5", "face4", "face3", "face2", "face1"];

/// `Sbar_DrawFace`'s powerup faces: invisibility+invulnerability, quad, invisibility,
/// invulnerability — checked in that priority order before the health face.
const FACE_INVIS_INVULN: &str = "face_inv2";
const FACE_QUAD: &str = "face_quad";
const FACE_INVIS: &str = "face_invis";
const FACE_INVULN: &str = "face_invul2";

/// Select the player-face health bracket exactly as `Sbar_DrawFace`:
/// `health >= 100 -> 4` (full-health `face1`); otherwise `health / 20` (integer
/// division). So 0..19 -> 0 (`face5`), 20..39 -> 1, 40..59 -> 2, 60..79 -> 3,
/// 80..99 -> 4, >=100 -> 4. A non-positive health (the player is dead — the C
/// shows the scorebar instead) clamps to bracket 0 so we never index out of range.
fn face_bracket(health: i32) -> usize {
    if health >= 100 {
        4
    } else if health <= 0 {
        0
    } else {
        ((health / 20) as usize).min(4)
    }
}

/// The selection-flash frame name for the *currently selected* weapon `i` (0..6),
/// keyed on the server `time` the orchestrator passes.
///
/// `Sbar_DrawInventory` cycles the active weapon through its 5 flash frames
/// `inva1_*..inva5_*` (`sb_weapons[2+f][i]`) right after selection. We do not
/// track per-item acquire times (only one `time` is supplied), so we run the same
/// 5-frame cycle continuously off `time`: `frame = (int)(time*10) % 5` in 0..4,
/// then the 1-based `inva{frame+1}_<suffix>` lump name. Non-selected owned weapons
/// use the dim `inv_*` name from [`WEAPON_INV_NAMES`] (handled by the caller).
// Retained for the future per-item `cl.item_gettime`-driven 1-second pickup flash;
// the steady-state HUD now draws the settled `inv2_*` icon for the active weapon.
#[allow(dead_code)]
fn weapon_flash_name(i: usize, time: f32) -> String {
    let suffix = WEAPON_SUFFIX.get(i).copied().unwrap_or("shotgun");
    let f = ((time * 10.0).floor() as i64).rem_euclid(5) + 1;
    format!("inva{f}_{suffix}")
}

/// Try to fetch a HUD pic by name and blit it at virtual `(vx, vy)`; a missing or
/// unparseable lump is silently skipped (`wad.qpic(name).ok()`), so the bar
/// degrades gracefully exactly as the task requires.
// Mirrors Sbar_DrawPic (sbar.c); the C reads vid/draw globals passed explicitly here.
#[allow(clippy::too_many_arguments)]
fn blit_named(
    image: &mut Image,
    wad: &crate::wad::Wad2,
    name: &str,
    vx: f32,
    vy: f32,
    scale: f32,
    vy_top: f32,
    palette: &[[u8; 3]; 256],
) {
    if name.is_empty() {
        return;
    }
    if let Ok(pic) = wad.qpic(name) {
        blit_qpic(image, &pic, vx, vy, scale, vy_top, palette);
    }
}

/// Stamp one console-font glyph (`conchars` cell `ch`) at virtual `(vx, vy)` in
/// 320x200 bar space, scaled/anchored exactly like [`blit_qpic`] — a port of
/// `Sbar_DrawCharacter`'s `Draw_Character`.
///
/// `conchars` is the raw 128x128 atlas wrapped as a [`crate::wad::Qpic`]
/// (`width = height = 128`), a 16x16 grid of 8x8 glyphs; byte `ch`'s glyph sits at
/// source `(8*(ch%16), 8*(ch/16))`. The ammo counts use the gold digit glyphs
/// `18 + digit` (cells 18..27). Glyph texels equal to palette index 0 are the
/// transparent background and are skipped; every write is clipped to the
/// framebuffer. The 8x8 glyph occupies an 8x8 *virtual* box, scaled to the frame.
// Mirrors Sbar_DrawCharacter/Draw_Character (sbar.c/draw.c); the C reads vid/draw
// globals passed explicitly here.
#[allow(clippy::too_many_arguments)]
fn draw_sbar_char(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    ch: u8,
    vx: f32,
    vy: f32,
    scale: f32,
    vy_top: f32,
    palette: &[[u8; 3]; 256],
) {
    if conchars.width != 128 || conchars.height != 128 || conchars.data.len() < 128 * 128 {
        return;
    }
    let cell_x = (ch as usize % 16) * 8;
    let cell_y = (ch as usize / 16) * 8;
    // Destination top-left in framebuffer pixels and the 8x8 scaled extent.
    let dst_x0 = (vx * scale).floor() as i64;
    let dst_y0 = (vy_top + vy * scale).floor() as i64;
    let dst_w = (8.0 * scale).round().max(1.0) as i64;
    let dst_h = (8.0 * scale).round().max(1.0) as i64;
    let inv_scale = 1.0 / scale;
    for dy in 0..dst_h {
        let py = dst_y0 + dy;
        if py < 0 || py >= image.h as i64 {
            continue;
        }
        let sy = (dy as f32 * inv_scale) as usize;
        if sy >= 8 {
            continue;
        }
        for dx in 0..dst_w {
            let px = dst_x0 + dx;
            if px < 0 || px >= image.w as i64 {
                continue;
            }
            let sx = (dx as f32 * inv_scale) as usize;
            if sx >= 8 {
                continue;
            }
            let texel = match conchars.data.get((cell_y + sy) * 128 + (cell_x + sx)) {
                Some(&t) => t,
                None => continue,
            };
            // conchars uses palette index 0 as the transparent glyph background.
            if texel == 0 {
                continue;
            }
            image.put(px as i32, py as i32, palette[texel as usize]);
        }
    }
}

/// Fetch the raw 128x128 `conchars` console font from `wad` as a [`crate::wad::Qpic`].
///
/// Unlike the HUD's `num_*`/`sbar` pics, `conchars` is a *headerless* lump (a flat
/// 128x128 byte block, no QPIC width/height prefix), so it is read via
/// `lump`/`lump_data` and wrapped with `width = height = 128` — exactly how
/// `quaketool`'s menu path builds it. A missing/short lump yields `None` and the
/// ammo-count text simply doesn't draw (graceful degrade).
pub fn conchars_pic(wad: &crate::wad::Wad2) -> Option<crate::wad::Qpic> {
    let lump = wad.lump("conchars")?;
    let data = wad.lump_data(lump).ok()?;
    if data.len() < 128 * 128 {
        return None;
    }
    Some(crate::wad::Qpic {
        width: 128,
        height: 128,
        data: data[..128 * 128].to_vec(),
    })
}

/// `Sbar_DrawInventory` (sbar.c): the `ibar` strip in the 24 virtual rows above
/// the status strip and, on it, the owned weapons, the four ammo counts, the
/// keys/powerups and the sigils. Called by [`draw_hud_into`] only while
/// `sb_lines > 24`. `scale` / `vy_top` are the bar's transform (see there).
fn draw_sbar_inventory(
    image: &mut Image,
    hud: &Hud,
    conchars: Option<&crate::wad::Qpic>,
    scale: f32,
    vy_top: f32,
) {
    let wad = hud.wad;
    let pal = hud.palette;
    // The `ibar` strip in the 24 rows above the sbar: Sbar_DrawPic(0, -24, sb_ibar).
    blit_named(image, wad, "ibar", 0.0, -24.0, scale, vy_top, pal);

    // Weapon icons: for each owned weapon (items bit IT_SHOTGUN<<i, i=0..6), draw
    // its icon at Sbar_DrawPic(i*24, -16, ...). The currently-selected weapon shows
    // the bright `inv2_*` icon, the rest the dim `inv_*` icon (Sbar_DrawInventory:
    // for `flashon >= 10` — i.e. >1s after pickup, the steady state — the active
    // weapon draws `sb_weapons[1][i]` = `inv2_*`). The 1-second post-pickup
    // `inva1..5` flash needs per-item `cl.item_gettime`, which we don't track, so we
    // render the settled bright icon the player sees the rest of the time.
    for i in 0..7 {
        let bit = IT_SHOTGUN << i;
        if hud.items & bit != 0 {
            let selected = hud.weapon == bit;
            if selected {
                let name = format!("inv2_{}", WEAPON_SUFFIX[i]);
                blit_named(image, wad, &name, (i as f32) * 24.0, -16.0, scale, vy_top, pal);
            } else {
                blit_named(image, wad, WEAPON_INV_NAMES[i], (i as f32) * 24.0, -16.0, scale, vy_top, pal);
            }
        }
    }

    // Ammo counts: the four totals (shells/nails/rockets/cells) in the top-right of
    // the ibar, small gold digits. Sbar_DrawInventory formats "%3i" (right-justified
    // in 3 chars) and draws each non-space char via Sbar_DrawCharacter at
    // ((6*i+1..3)*8 - 2, -24) using glyph `18 + digit` (the gold conchars digits).
    if let Some(cc) = conchars {
        let counts = [hud.ammo_shells, hud.ammo_nails, hud.ammo_rockets, hud.ammo_cells];
        for (i, &count) in counts.iter().enumerate() {
            // "%3i": right-justified, blanks for leading zeros, clamped to >=0.
            let s = format!("{:3}", count.max(0));
            let b = s.as_bytes();
            for (j, &c) in b.iter().enumerate() {
                if c == b' ' {
                    continue;
                }
                // Gold digit glyph 18 + (c - '0'); x = (6*i + 1 + j)*8 - 2, y = -24.
                let glyph = 18 + (c - b'0');
                let vx = ((6 * i + 1 + j) as f32) * 8.0 - 2.0;
                draw_sbar_char(image, cc, glyph, vx, -24.0, scale, vy_top, pal);
            }
        }
    }

    // Items: keys + powerups (sb_items[0..5]) for items bits 1<<(17+i), at
    // Sbar_DrawPic(192 + i*16, -16, ...). Then sigils (sb_sigil[0..3]) for items
    // bits 1<<(28+i) at Sbar_DrawPic(320-32 + i*8, -16, ...).
    for (i, name) in SB_ITEM_NAMES.iter().enumerate() {
        if hud.items & (1 << (17 + i)) != 0 {
            blit_named(image, wad, name, 192.0 + (i as f32) * 16.0, -16.0, scale, vy_top, pal);
        }
    }
    for (i, name) in SB_SIGIL_NAMES.iter().enumerate() {
        if hud.items & (1 << (28 + i)) != 0 {
            blit_named(image, wad, name, 320.0 - 32.0 + (i as f32) * 8.0, -16.0, scale, vy_top, pal);
        }
    }
}

/// Draw the Quake status bar (HUD) across the bottom of `image`, on top of the
/// finished 3-D frame — a faithful port of `sbar.c`'s `Sbar_Draw` (single-player /
/// non-deathmatch path).
///
/// The whole bar is laid out in Quake's fixed 320x200 virtual space and scaled by
/// `image.w / 320` (nearest-neighbour) so it spans the full framebuffer width,
/// bottom-anchored. The *status area* is 48 virtual rows tall: the `ibar`
/// inventory strip (320x24) sits in the 24 rows ABOVE the `sbar` (320x24)
/// status strip — matching `Sbar_DrawPic(0, -24, sb_ibar)` (the C draws relative
/// to `vid.height - SBAR_HEIGHT`, so a virtual `y` maps straight to our `vy`).
///
/// How much of it draws follows `hud.sb_lines` ([`calc_refdef`]): the inventory
/// strip only above 24 lines, the status strip only above 0 — but the death /
/// Tab scoreboard (`scorebar`) regardless, exactly like `Sbar_Draw`.
///
/// Drawing order (mirrors `Sbar_Draw` → `Sbar_DrawInventory` then the sbar block):
///  1. `ibar` strip, then on it: owned weapon icons (the selected one flashing its
///     `inva*` frames), the four small ammo counts, keys/powerups, and sigils.
///  2. `sbar` strip, then on it: the armour-type icon + armour number (left), the
///     animated player face (centre, x=112), the health number, the ammo-type
///     icon (x=224) and the current-ammo number (right).
///
/// Every pic is fetched via `wad.qpic(name).ok()` (and `conchars` via
/// `lump_data`), so a `gfx.wad` missing any element degrades gracefully — that
/// element just doesn't draw, never a panic and never an errored frame.
pub fn draw_hud_into(image: &mut Image, hud: &Hud) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    // Scale the 320-wide virtual layout to the real framebuffer width.
    let scale = image.w as f32 / HUD_VIRT_W;
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    // Framebuffer y of virtual row 0 of the bar (top of the `sbar` strip); the
    // 24-px sbar sits flush at the bottom, the ibar 24 rows above it (negative vy).
    let vy_top = image.h as f32 - HUD_BAR_H * scale;
    let wad = hud.wad;
    let pal = hud.palette;
    let conchars = conchars_pic(wad);

    // ----- Inventory bar (Sbar_DrawInventory) -------------------------------
    // Sbar_Draw: `if (sb_lines > 24) Sbar_DrawInventory ();` — viewsize 110
    // (sb_lines 24) keeps only the status strip, 120 (0) neither.
    if hud.sb_lines > 24 {
        draw_sbar_inventory(image, hud, conchars.as_ref(), scale, vy_top);
    }

    // ----- Status bar (the sbar block of Sbar_Draw) -------------------------
    // When the player is dead (health <= 0) or holding Tab, the C replaces the whole
    // status strip with the dark `scorebar` pic + the solo scoreboard
    // (Monsters/Secrets/Time/level), keeping the ibar above. (sbar.c:948-953.)
    if hud.health <= 0 || hud.show_scores {
        blit_named(image, wad, "scorebar", 0.0, 0.0, scale, vy_top, pal);
        if let Some(cc) = &conchars {
            draw_solo_scoreboard(image, cc, hud, scale, vy_top, pal);
        }
        return;
    }
    // `else if (sb_lines)`: no status strip at viewsize 120.
    if hud.sb_lines <= 0 {
        return;
    }

    // 1. Background strip (sbar, 320x24) at virtual (0,0).
    blit_named(image, wad, "sbar", 0.0, 0.0, scale, vy_top, pal);

    // Armour field (Sbar_Draw, sbar.c:968-997). Under invulnerability the C draws a
    // gold "666" and the Pentagram-of-Protection disc over the armour slot and shows
    // NO real armour icon/number; otherwise the armour-type icon (Sbar_DrawPic(0, 0,
    // sb_armor[type])) keyed on IT_ARMOR3/2/1 plus the armour number at
    // Sbar_DrawNum(24, ..) — right edge virtual x=96, gold when <=25.
    if hud.items & IT_INVULNERABILITY != 0 {
        draw_num(image, 666, 96.0, 0.0, scale, vy_top, wad, pal, true);
        blit_named(image, wad, "disc", 0.0, 0.0, scale, vy_top, pal);
    } else {
        if hud.items & IT_ARMOR3 != 0 {
            blit_named(image, wad, ARMOR_ICON_NAMES[2], 0.0, 0.0, scale, vy_top, pal);
        } else if hud.items & IT_ARMOR2 != 0 {
            blit_named(image, wad, ARMOR_ICON_NAMES[1], 0.0, 0.0, scale, vy_top, pal);
        } else if hud.items & IT_ARMOR1 != 0 {
            blit_named(image, wad, ARMOR_ICON_NAMES[0], 0.0, 0.0, scale, vy_top, pal);
        }
        draw_num(image, hud.armor, 96.0, 0.0, scale, vy_top, wad, pal, hud.armor <= 25);
    }

    // Face (Sbar_DrawFace) at x=112, y=0. Powerup faces take priority in the C's
    // order: invisibility+invulnerability, then quad, then invisibility, then
    // invulnerability; otherwise the health-bracket face (pain frame skipped — we
    // don't track faceanimtime, so we use the static face[bracket][0]).
    let inv_iv = IT_INVISIBILITY | IT_INVULNERABILITY;
    if hud.items & inv_iv == inv_iv {
        blit_named(image, wad, FACE_INVIS_INVULN, 112.0, 0.0, scale, vy_top, pal);
    } else if hud.items & IT_QUAD != 0 {
        blit_named(image, wad, FACE_QUAD, 112.0, 0.0, scale, vy_top, pal);
    } else if hud.items & IT_INVISIBILITY != 0 {
        blit_named(image, wad, FACE_INVIS, 112.0, 0.0, scale, vy_top, pal);
    } else if hud.items & IT_INVULNERABILITY != 0 {
        blit_named(image, wad, FACE_INVULN, 112.0, 0.0, scale, vy_top, pal);
    } else {
        let face = FACE_NAMES[face_bracket(hud.health)];
        blit_named(image, wad, face, 112.0, 0.0, scale, vy_top, pal);
    }

    // Health number: Sbar_DrawNum(136, health, 3, health<=25) — right edge x=208.
    draw_num(image, hud.health, 208.0, 0.0, scale, vy_top, wad, pal, hud.health <= 25);

    // Ammo-type icon (Sbar_DrawPic(224, 0, sb_ammo[type])) by the active weapon's
    // ammo type, keyed on the items ammo bits IT_SHELLS/NAILS/ROCKETS/CELLS.
    if hud.items & IT_SHELLS != 0 {
        blit_named(image, wad, AMMO_ICON_NAMES[0], 224.0, 0.0, scale, vy_top, pal);
    } else if hud.items & IT_NAILS != 0 {
        blit_named(image, wad, AMMO_ICON_NAMES[1], 224.0, 0.0, scale, vy_top, pal);
    } else if hud.items & IT_ROCKETS != 0 {
        blit_named(image, wad, AMMO_ICON_NAMES[2], 224.0, 0.0, scale, vy_top, pal);
    } else if hud.items & IT_CELLS != 0 {
        blit_named(image, wad, AMMO_ICON_NAMES[3], 224.0, 0.0, scale, vy_top, pal);
    }

    // Current ammo number: Sbar_DrawNum(248, ammo, 3, ammo<=10) — right edge x=320.
    draw_num(image, hud.ammo, 320.0, 0.0, scale, vy_top, wad, pal, hud.ammo <= 10);
}

/// `Sbar_SoloScoreboard` (sbar.c:457): the single-player stats drawn over the
/// `scorebar` strip on death / Tab — kills, secrets, elapsed time, and the level
/// name. Positions are verbatim from the C (virtual sbar-space, y in 0..24): the
/// "Monsters" / "Secrets" lines at x=8 (rows 4, 12), "Time" at x=184 row 4, and the
/// level name right-justified ending at virtual x≈232 on row 12 (`232 - len*4`).
fn draw_solo_scoreboard(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    hud: &Hud,
    scale: f32,
    vy_top: f32,
    pal: &[[u8; 3]; 256],
) {
    let draw = |image: &mut Image, vx: f32, vy: f32, s: &str| {
        for (i, &c) in s.as_bytes().iter().enumerate() {
            // Sbar_DrawString blits the raw ASCII glyph (space included, harmless).
            draw_sbar_char(image, conchars, c, vx + (i as f32) * 8.0, vy, scale, vy_top, pal);
        }
    };
    draw(
        image,
        8.0,
        4.0,
        &format!("Monsters:{:3} /{:3}", hud.monsters, hud.total_monsters),
    );
    draw(
        image,
        8.0,
        12.0,
        &format!("Secrets :{:3} /{:3}", hud.secrets, hud.total_secrets),
    );
    // Time: minutes:tens-units from the server clock (sbar.c uses integer seconds).
    let t = hud.time.max(0.0) as i32;
    let minutes = t / 60;
    let seconds = t - 60 * minutes;
    let tens = seconds / 10;
    let units = seconds - 10 * tens;
    draw(image, 184.0, 4.0, &format!("Time :{minutes:3}:{tens}{units}"));
    // Level name, right-justified to end at virtual x≈232 (232 - len*4 start).
    let l = hud.level_name.len() as f32;
    draw(image, 232.0 - l * 4.0, 12.0, hud.level_name);
}

// ---------------------------------------------------------------------------
// Intermission + finale overlays (sbar.c Sbar_IntermissionOverlay /
// Sbar_FinaleOverlay + screen.c SCR_DrawCenterString's finale char reveal)
// ---------------------------------------------------------------------------

/// The level-complete numbers `Sbar_IntermissionOverlay` (sbar.c) draws over the
/// `gfx/inter.lmp` plaque: the completion time and the secrets/monsters counts
/// (`cl.completed_time`, `cl.stats[STAT_SECRETS/TOTALSECRETS/MONSTERS/
/// TOTALMONSTERS]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntermissionStats {
    /// `cl.completed_time` in whole seconds (latched when svc_intermission arrived).
    pub completed_time: i32,
    /// Found secrets (`found_secrets` QuakeC global / STAT_SECRETS).
    pub secrets: i32,
    /// Total secrets in the level (`total_secrets` / STAT_TOTALSECRETS).
    pub total_secrets: i32,
    /// Killed monsters (`killed_monsters` / STAT_MONSTERS).
    pub monsters: i32,
    /// Total monsters in the level (`total_monsters` / STAT_TOTALMONSTERS).
    pub total_monsters: i32,
}

/// `Sbar_IntermissionNumber` (sbar.c): draw `num` with the big white `num_*`
/// digit pics, right-justified into `digits` 24-px slots whose LEFT edge is at
/// virtual `vx` (a number shorter than `digits` starts `(digits-l)*24` further
/// right; a longer one keeps only its trailing `digits` digits). Virtual
/// coordinates in the 320x200 space, mapped by `scale`/`ox`/`oy` like the menu.
#[allow(clippy::too_many_arguments)]
fn intermission_number(
    image: &mut Image,
    wad: &crate::wad::Wad2,
    palette: &[[u8; 3]; 256],
    num: i32,
    vx: f32,
    vy: f32,
    digits: usize,
    scale: f32,
    ox: f32,
    oy: f32,
) {
    // Sbar_itoa renders the (possibly negative) value: a leading '-' draws as
    // the STAT_MINUS glyph — sb_nums[0][10], the `num_minus` wad pic (Sbar_Init).
    // Rust's `to_string` yields exactly the C's sign-then-digits form.
    let s = num.to_string();
    let b = s.as_bytes();
    let shown = if b.len() > digits { &b[b.len() - digits..] } else { b };
    let mut x = vx + (digits.saturating_sub(shown.len())) as f32 * 24.0;
    for &c in shown {
        let name = if c == b'-' { "num_minus" } else { NUM_NAMES[(c - b'0') as usize] };
        if let Ok(pic) = wad.qpic(name) {
            blit_qpic_at(image, &pic, x, vy, scale, ox, oy, palette);
        }
        x += 24.0; // the C steps a fixed 24 per digit slot
    }
}

/// `Sbar_IntermissionOverlay` (sbar.c): the single-player level-complete screen —
/// the `gfx/complete.lmp` banner at (64,24), the `gfx/inter.lmp` plaque at (0,56),
/// and the big-number time (minutes:seconds), secrets found/total and monsters
/// killed/total beside the plaque's labels. Drawn in the 320x200 virtual space,
/// uniformly scaled and centered like the menu (`min(w/320, h/200)`); on the
/// engine's 16:10 presets that equals the HUD's `w/320` with zero offset.
///
/// `complete`/`inter` are the two pak pics (`Draw_CachePic` in the C); either
/// being absent just skips that blit — the numbers still draw, never a panic.
/// The big digits and the colon/slash come from `gfx.wad` like the HUD's.
pub fn draw_intermission_overlay(
    image: &mut Image,
    wad: &crate::wad::Wad2,
    palette: &[[u8; 3]; 256],
    complete: Option<&crate::wad::Qpic>,
    inter: Option<&crate::wad::Qpic>,
    stats: &IntermissionStats,
) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let sx = image.w as f32 / MENU_VIRT_W;
    let sy = image.h as f32 / MENU_VIRT_H;
    let scale = sx.min(sy);
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    let ox = (image.w as f32 - MENU_VIRT_W * scale) * 0.5;
    let oy = (image.h as f32 - MENU_VIRT_H * scale) * 0.5;

    // Draw_Pic(64, 24, "gfx/complete.lmp") — the "Level Complete" banner.
    if let Some(pic) = complete {
        blit_qpic_at(image, pic, 64.0, 24.0, scale, ox, oy, palette);
    }
    // Draw_TransPic(0, 56, "gfx/inter.lmp") — the Time/Secrets/Kills plaque.
    if let Some(pic) = inter {
        blit_qpic_at(image, pic, 0.0, 56.0, scale, ox, oy, palette);
    }

    // Time: minutes right-justified at (160,64) over 3 slots, then num_colon at
    // 234 and the two second digits at 246/266 (verbatim sbar.c coordinates).
    // DEVIATION: clamped at 0 — a negative time would make the C's direct
    // `sb_nums[0][num/10]` second-digit lookups index negatively (UB); the
    // signed stats rows below go through intermission_number's minus glyph.
    let t = stats.completed_time.max(0);
    let minutes = t / 60;
    let seconds = t - 60 * minutes;
    intermission_number(image, wad, palette, minutes, 160.0, 64.0, 3, scale, ox, oy);
    if let Ok(pic) = wad.qpic("num_colon") {
        blit_qpic_at(image, &pic, 234.0, 64.0, scale, ox, oy, palette);
    }
    if let Ok(pic) = wad.qpic(NUM_NAMES[(seconds / 10) as usize]) {
        blit_qpic_at(image, &pic, 246.0, 64.0, scale, ox, oy, palette);
    }
    if let Ok(pic) = wad.qpic(NUM_NAMES[(seconds % 10) as usize]) {
        blit_qpic_at(image, &pic, 266.0, 64.0, scale, ox, oy, palette);
    }

    // Secrets: found at (160,104), num_slash at 232, total at 240.
    intermission_number(image, wad, palette, stats.secrets, 160.0, 104.0, 3, scale, ox, oy);
    if let Ok(pic) = wad.qpic("num_slash") {
        blit_qpic_at(image, &pic, 232.0, 104.0, scale, ox, oy, palette);
    }
    intermission_number(image, wad, palette, stats.total_secrets, 240.0, 104.0, 3, scale, ox, oy);

    // Monsters: killed at (160,144), num_slash at 232, total at 240.
    intermission_number(image, wad, palette, stats.monsters, 160.0, 144.0, 3, scale, ox, oy);
    if let Ok(pic) = wad.qpic("num_slash") {
        blit_qpic_at(image, &pic, 232.0, 144.0, scale, ox, oy, palette);
    }
    intermission_number(image, wad, palette, stats.total_monsters, 240.0, 144.0, 3, scale, ox, oy);
}

/// `SCR_DrawCenterString` (screen.c) in its finale mode: the centered text block
/// revealed one character at a time. `remaining` is the C's
/// `scr_printspeed.value * (cl.time - scr_centertime_start)` budget — note the
/// `if (!remaining--) return;` runs AFTER each `Draw_Character`, so a budget of
/// `n` paints `n + 1` characters (one appears the instant the finale starts).
/// A NEGATIVE budget paints the whole string: `!remaining--` only fires when
/// `remaining` is exactly 0 at the check, and a below-zero value just keeps
/// decrementing past it. Each line is scanned at most 40 characters (longer
/// lines truncate and skip to the next `\n`), the block starts at
/// `vid.height*0.35` for <= 4 lines else 48, and every line centers
/// independently — all verbatim from the C.
pub fn draw_center_string_revealed(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    palette: &[[u8; 3]; 256],
    text: &str,
    remaining: i32,
) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let sx = image.w as f32 / MENU_VIRT_W;
    let sy = image.h as f32 / MENU_VIRT_H;
    let scale = sx.min(sy);
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    let ox = (image.w as f32 - MENU_VIRT_W * scale) * 0.5;
    let oy = (image.h as f32 - MENU_VIRT_H * scale) * 0.5;

    let lines: Vec<&str> = text.split('\n').collect();
    // scr_center_lines <= 4 => y = vid.height*0.35 (virtual 70); taller => 48.
    let mut vy = if lines.len() <= 4 { 200.0 * 0.35 } else { 48.0 };
    let mut budget = remaining;
    for line in lines {
        // The C scans the line width up to 40 characters.
        let bytes = line.as_bytes();
        let l = bytes.len().min(40);
        let vx = (MENU_VIRT_W - l as f32 * 8.0) * 0.5;
        for (j, &c) in bytes[..l].iter().enumerate() {
            draw_char_scaled(image, conchars, vx + j as f32 * 8.0, vy, c, scale, ox, oy, palette);
            if budget == 0 {
                return; // `if (!remaining--) return;` — this char was the last.
            }
            // Post-decrement: a budget already below zero just keeps falling
            // (never re-hits the `== 0` gate), painting the whole string like
            // the C. `wrapping_sub` keeps even an `i32::MIN` caller total.
            budget = budget.wrapping_sub(1);
        }
        vy += 8.0;
    }
}

/// `Sbar_FinaleOverlay` (sbar.c) + the finale half of `SCR_DrawCenterString`
/// (screen.c): the horizontally-centered `gfx/finale.lmp` plaque at y=16 and the
/// episode-end text revealed at `scr_printspeed` (8) characters per second of
/// `elapsed` (`cl.time - scr_centertime_start`). Pass `finale_pic = None` for
/// `svc_cutscene` (`cl.intermission == 3`), which draws the text alone.
pub fn draw_finale_overlay(
    image: &mut Image,
    conchars: Option<&crate::wad::Qpic>,
    palette: &[[u8; 3]; 256],
    finale_pic: Option<&crate::wad::Qpic>,
    text: &str,
    elapsed: f32,
) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let sx = image.w as f32 / MENU_VIRT_W;
    let sy = image.h as f32 / MENU_VIRT_H;
    let scale = sx.min(sy);
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    let ox = (image.w as f32 - MENU_VIRT_W * scale) * 0.5;
    let oy = (image.h as f32 - MENU_VIRT_H * scale) * 0.5;

    // Draw_TransPic((vid.width - pic->width)/2, 16, "gfx/finale.lmp").
    if let Some(pic) = finale_pic {
        let vx = (MENU_VIRT_W - pic.width.max(0) as f32) * 0.5;
        blit_qpic_at(image, pic, vx, 16.0, scale, ox, oy, palette);
    }
    // scr_printspeed defaults to "8" (screen.c): 8 characters per second.
    if let Some(cc) = conchars {
        let remaining = (8.0 * elapsed.max(0.0)).min(9999.0) as i32;
        draw_center_string_revealed(image, cc, palette, text, remaining);
    }
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
/// `OPTIONS_ITEMS` (menu.c, the non-Win32 layout = 13 rows). The cursor wraps over
/// all 13; the row indices match `M_AdjustSliders` / `M_Options_Key`:
/// 0 Customize controls, 1 Go to console, 2 Reset to defaults, 3 Screen size,
/// 4 Brightness, 5 Mouse Speed, 6 CD Music Volume, 7 Sound Volume, 8 Always Run,
/// 9 Invert Mouse, 10 Lookspring, 11 Lookstrafe, 12 Video Options.
const OPTIONS_ITEMS: usize = 13;

/// `OptionRow` — the stable index for each Options row (matches the C's
/// `options_cursor` cases in `M_AdjustSliders` / `M_Options_Key`).
const ROW_CONTROLS: usize = 0;
const ROW_CONSOLE: usize = 1;
const ROW_DEFAULTS: usize = 2;
const ROW_SCREENSIZE: usize = 3;
const ROW_BRIGHTNESS: usize = 4;
const ROW_MOUSESPEED: usize = 5;
const ROW_CDVOLUME: usize = 6;
const ROW_SNDVOLUME: usize = 7;
const ROW_ALWAYSRUN: usize = 8;
const ROW_INVERTMOUSE: usize = 9;
const ROW_LOOKSPRING: usize = 10;
const ROW_LOOKSTRAFE: usize = 11;
const ROW_VIDEO: usize = 12;

/// `MULTIPLAYER_ITEMS` (menu.c): the multiplayer menu has 3 entries (Join /
/// New Game / Setup). Netcode is out of scope for this port, so like the C with
/// no net drivers, Enter on Join/New Game does nothing and the screen shows the
/// "No Communications Available" line (`M_MultiPlayer_Draw`).
const MULTIPLAYER_ITEMS: usize = 3;

/// `MAX_SAVEGAMES` (quakedef.h): the Load/Save menus list 12 slots.
pub const MAX_SAVEGAMES: usize = 12;
/// The text `M_ScanSaves` (menu.c) puts in every slot without an `sN.sav` file.
/// A slot whose host-set comment is empty shows exactly this.
pub const UNUSED_SLOT: &str = "--- UNUSED SLOT ---";

// --- key bindings (menu.c M_Keys_*, keys.h/keys.c) -----------------------------

/// `bindnames` (menu.c): the (command, label) rows `M_Keys_Draw` lists, verbatim.
pub const BINDNAMES: [(&str, &str); NUM_BINDNAMES] = [
    ("+attack", "attack"),
    ("impulse 10", "change weapon"),
    ("+jump", "jump / swim up"),
    ("+forward", "walk forward"),
    ("+back", "backpedal"),
    ("+left", "turn left"),
    ("+right", "turn right"),
    ("+speed", "run"),
    ("+moveleft", "step left"),
    ("+moveright", "step right"),
    ("+strafe", "sidestep"),
    ("+lookup", "look up"),
    ("+lookdown", "look down"),
    ("centerview", "center view"),
    ("+mlook", "mouse look"),
    ("+klook", "keyboard look"),
    ("+moveup", "swim up"),
    ("+movedown", "swim down"),
];
/// `NUMCOMMANDS` (menu.c): the bindnames row count.
pub const NUM_BINDNAMES: usize = 18;

/// Indices into [`BINDNAMES`] for the commands the host actually drives (the
/// rest are list-only: `+mlook` is permanent under pointer lock and `+klook`
/// has no effect without keyboard-look pitch — both still draw + rebind
/// faithfully).
pub const BIND_ATTACK: usize = 0;
pub const BIND_CHANGEWEAPON: usize = 1;
pub const BIND_JUMP: usize = 2;
pub const BIND_FORWARD: usize = 3;
pub const BIND_BACK: usize = 4;
pub const BIND_LEFT: usize = 5;
pub const BIND_RIGHT: usize = 6;
pub const BIND_SPEED: usize = 7;
pub const BIND_MOVELEFT: usize = 8;
pub const BIND_MOVERIGHT: usize = 9;
pub const BIND_STRAFE: usize = 10;
pub const BIND_LOOKUP: usize = 11;
pub const BIND_LOOKDOWN: usize = 12;
pub const BIND_CENTERVIEW: usize = 13;
pub const BIND_MOVEUP: usize = 16;
pub const BIND_MOVEDOWN: usize = 17;
/// Commands `default.cfg` binds that `M_Keys_Draw` doesn't list: they sit past
/// the [`BINDNAMES`] rows, so Customize controls never shows them, but a
/// rebind over their key or `Reset to defaults` treats them like any other
/// binding. `bind + "sizeup"`, `bind = "sizeup"`, `bind - "sizedown"`.
pub const BIND_SIZEUP: usize = NUM_BINDNAMES;
pub const BIND_SIZEDOWN: usize = NUM_BINDNAMES + 1;

/// Quake key numbers (keys.h): printable ASCII is itself; the special keys take
/// the 128+ block. Only the keys a browser page can sensibly deliver are named
/// here; the bindings table spans the full `0..256` like the C `keybindings`.
pub const K_TAB: u8 = 9;
pub const K_ENTER: u8 = 13;
pub const K_ESCAPE: u8 = 27;
pub const K_SPACE: u8 = 32;
pub const K_BACKSPACE: u8 = 127;
pub const K_UPARROW: u8 = 128;
pub const K_DOWNARROW: u8 = 129;
pub const K_LEFTARROW: u8 = 130;
pub const K_RIGHTARROW: u8 = 131;
pub const K_ALT: u8 = 132;
pub const K_CTRL: u8 = 133;
pub const K_SHIFT: u8 = 134;
pub const K_F1: u8 = 135;
pub const K_F12: u8 = 146;
pub const K_INS: u8 = 147;
pub const K_DEL: u8 = 148;
pub const K_PGDN: u8 = 149;
pub const K_PGUP: u8 = 150;
pub const K_HOME: u8 = 151;
pub const K_END: u8 = 152;
pub const K_MOUSE1: u8 = 200;
pub const K_MOUSE2: u8 = 201;
pub const K_MOUSE3: u8 = 202;

/// `Key_KeynumToString` (keys.c): printable ASCII (33..=126) is the character
/// itself (lowercase, as `Key_Event` delivers it); the named specials come from
/// the `keynames` table; anything else is the C's `<UNKNOWN KEYNUM>` (shortened
/// to fit the 320-wide menu column).
pub fn keynum_to_string(keynum: u8) -> String {
    if keynum > 32 && keynum < 127 {
        return (keynum as char).to_string();
    }
    match keynum {
        K_TAB => "TAB",
        K_ENTER => "ENTER",
        K_ESCAPE => "ESCAPE",
        K_SPACE => "SPACE",
        K_BACKSPACE => "BACKSPACE",
        K_UPARROW => "UPARROW",
        K_DOWNARROW => "DOWNARROW",
        K_LEFTARROW => "LEFTARROW",
        K_RIGHTARROW => "RIGHTARROW",
        K_ALT => "ALT",
        K_CTRL => "CTRL",
        K_SHIFT => "SHIFT",
        K_INS => "INS",
        K_DEL => "DEL",
        K_PGDN => "PGDN",
        K_PGUP => "PGUP",
        K_HOME => "HOME",
        K_END => "END",
        K_MOUSE1 => "MOUSE1",
        K_MOUSE2 => "MOUSE2",
        K_MOUSE3 => "MOUSE3",
        f @ K_F1..=K_F12 => return format!("F{}", f - K_F1 + 1),
        _ => "UNKNOWN",
    }
    .to_string()
}

/// The boot key bindings: id's `default.cfg` (from the pak) for every key the
/// page delivers, PLUS this port's established WASD layout (the shareware
/// `default.cfg` predates WASD — it binds `a` to `+lookup` and `d` to `+moveup`;
/// this port has always shipped WASD movement, so WASD overrides those four,
/// exactly as a player's `config.cfg` would).
fn default_bindings() -> [Option<u8>; 256] {
    let mut b: [Option<u8>; 256] = [None; 256];
    let mut bind = |key: u8, cmd: usize| b[key as usize] = Some(cmd as u8);
    // default.cfg (id, verbatim — the keys the page can deliver):
    bind(K_ALT, BIND_STRAFE);
    bind(b',', BIND_MOVELEFT);
    bind(b'.', BIND_MOVERIGHT);
    bind(K_DEL, BIND_LOOKDOWN);
    bind(K_PGDN, BIND_LOOKUP);
    bind(K_END, BIND_CENTERVIEW);
    bind(b'z', BIND_LOOKDOWN);
    bind(K_SHIFT, BIND_SPEED);
    bind(b'+', BIND_SIZEUP);
    bind(b'=', BIND_SIZEUP);
    bind(b'-', BIND_SIZEDOWN);
    bind(K_CTRL, BIND_ATTACK);
    bind(K_UPARROW, BIND_FORWARD);
    bind(K_DOWNARROW, BIND_BACK);
    bind(K_LEFTARROW, BIND_LEFT);
    bind(K_RIGHTARROW, BIND_RIGHT);
    bind(K_SPACE, BIND_JUMP);
    bind(b'/', BIND_CHANGEWEAPON);
    bind(K_MOUSE1, BIND_ATTACK);
    // This port's established layout (overrides default.cfg's a=+lookup,
    // d=+moveup; w/s were unbound there):
    bind(b'w', BIND_FORWARD);
    bind(b's', BIND_BACK);
    bind(b'a', BIND_MOVELEFT);
    bind(b'd', BIND_MOVERIGHT);
    bind(b'c', BIND_MOVEDOWN);
    b
}

/// The video modes the Video Options screen (`M_Video` -> `VID_MenuDraw`) lists,
/// as `(width, height)` render resolutions — this port's `modelist`. A
/// consistent 16:10 ladder (each step +160w/+100h) from the fast `320x200` up to
/// the host's `1280x800` clamp cap (`1_280*800` = the exact pixel budget). The
/// engine *boots* at the host's chosen default (see wasm `DEFAULT_W`/`DEFAULT_H`),
/// which must be one of these so the list can mark the current mode; higher modes
/// render the 3-D scene at the larger size (the menu + HUD auto-scale to whatever
/// framebuffer they're drawn into). The Options "Screen size" row is id's
/// `viewsize` (see [`calc_refdef`]), not the mode, exactly as in WinQuake.
pub const RESOLUTION_PRESETS: [(i32, i32); 7] = [
    (320, 200),
    (480, 300),
    (640, 400),
    (800, 500),
    (960, 600),
    (1120, 700),
    (1280, 800),
];

// --- analog cvar ranges (M_AdjustSliders) + their slider fraction mapping ------

/// `sensitivity` (Mouse Speed): 1..=11, step 0.5; slider r = (v-1)/10. Default 3.
const SENS_MIN: f32 = 1.0;
const SENS_MAX: f32 = 11.0;
const SENS_STEP: f32 = 0.5;
const SENS_DEFAULT: f32 = 3.0;

/// `volume` (Sound Volume): 0..=1, step 0.1; slider r = v. Default 0.7.
const VOLUME_MIN: f32 = 0.0;
const VOLUME_MAX: f32 = 1.0;
const VOLUME_STEP: f32 = 0.1;
const VOLUME_DEFAULT: f32 = 0.7;

/// `v_gamma` (Brightness): 0.5..=1, step 0.05 (RIGHT brightens: the C does
/// `v_gamma.value -= dir * 0.05`); slider r = (1 - v)/0.5. Default 1.0. LIVE:
/// the host runs the presented frame through [`build_gamma_table`] (the C
/// applies `gammatable` at the hardware-palette boundary,
/// `V_UpdatePalette` -> `VID_ShiftPalette`); 1.0 is a byte-exact identity.
const GAMMA_MIN: f32 = 0.5;
const GAMMA_MAX: f32 = 1.0;
const GAMMA_STEP: f32 = 0.05;
const GAMMA_DEFAULT: f32 = 1.0;

/// `bgmvolume` (CD Music Volume): 0..=1, step 0.1; slider r = v. Default 1.0.
/// The slider is live (stores the cvar, exposed via [`Menu::bgm_volume`]).
/// DEVIATION (scope): there is no CD audio device in this port, so no track
/// ever plays at this volume — exactly like the C run without a CD, where the
/// cvar still adjusts (cd_null.c).
const BGM_MIN: f32 = 0.0;
const BGM_MAX: f32 = 1.0;
const BGM_STEP: f32 = 0.1;
const BGM_DEFAULT: f32 = 1.0;

/// Build the 256-entry gamma LUT, a port of `BuildGammaTable` (view.c):
/// `gammatable[i] = 255 * pow((i+0.5)/255.5, g) + 0.5`, clamped to `0..=255` —
/// and the C's exact `g == 1.0` special case, a literal identity table (so the
/// default gamma is BYTE-EXACT, not merely close). The host applies this where
/// the finished frame becomes presented RGB, the same boundary as the C's
/// `V_UpdatePalette` -> `VID_ShiftPalette` hardware-palette write (gamma there
/// runs AFTER the cshift blend; the host matches that order). quaketool's PPM
/// scene path never applies it (the C's default boot state), so the golden
/// renders are untouched.
pub fn build_gamma_table(g: f32) -> [u8; 256] {
    let mut table = [0u8; 256];
    if g == 1.0 {
        for (i, t) in table.iter_mut().enumerate() {
            *t = i as u8;
        }
        return table;
    }
    for (i, t) in table.iter_mut().enumerate() {
        // The C computes pow in double and truncates the +0.5-rounded value to
        // int, then clamps; mirror that exactly.
        let inf = (255.0 * ((i as f64 + 0.5) / 255.5).powf(g as f64) + 0.5) as i32;
        *t = inf.clamp(0, 255) as u8;
    }
    table
}

/// `NUM_HELP_PAGES` (menu.c): the Help/Ordering screen pages through
/// `gfx/help0.lmp`..`help5.lmp`.
pub const NUM_HELP_PAGES: usize = 6;

/// The map New Game starts on. Matches the C `map start`: `start.bsp` is the
/// skill-select hub — the player walks into the Easy/Normal/Hard/Nightmare halls
/// (`trigger_setskill`) and an episode slipgate (`trigger_changelevel`) that
/// changelevels into `e1m1` (or e2m1/e3m1/e4m1). Changelevel is implemented, so
/// the full hub flow works.
pub const NEW_GAME_MAP: &str = "maps/start.bsp";

// --- conchars glyph indices used by the Options widgets (M_DrawSlider /
//     M_DrawCheckbox) and the cursor (M_Options_Draw). ----------------------------

/// `M_DrawSlider`: the slider trough is glyph 128 (left cap), 129 (the middle
/// segment, repeated [`SLIDER_RANGE`] times), then 130 (right cap); the knob is
/// glyph 131. The whole widget is drawn at virtual x=220.
const SLIDER_LEFT_CHAR: u8 = 128;
const SLIDER_MID_CHAR: u8 = 129;
const SLIDER_RIGHT_CHAR: u8 = 130;
const SLIDER_KNOB_CHAR: u8 = 131;
/// `SLIDER_RANGE` (menu.c): the slider trough is 10 middle segments wide.
const SLIDER_RANGE: usize = 10;

/// The Options cursor: glyph `12 + ((realtime*4)&1)` (12/13 flash) drawn at
/// virtual x=200 (`M_DrawCharacter(200, 32 + cursor*8, 12 + ...)`).
const OPTIONS_CURSOR_BASE: u8 = 12;
const OPTIONS_CURSOR_X: f32 = 200.0;

/// `(int)(realtime*rate) & 1` — the C's cursor-flash bit, shared by the menu
/// cursors (`rate` 4) and the console input cursor (`con_cursorspeed` 4). The C
/// truncates the double product toward zero; `realtime` only grows, so a
/// non-finite or non-positive clock is phase 0.
fn realtime_blink_bit(realtime: f64, rate: f64) -> u8 {
    if !realtime.is_finite() || realtime <= 0.0 {
        return 0;
    }
    ((realtime * rate) as i64 & 1) as u8
}

/// The flashing menu cursor's conchars cell: `12 + ((int)(realtime*4) & 1)`,
/// verbatim from every text menu that has one (`M_Options_Draw`,
/// `M_Load_Draw`/`M_Save_Draw`, `M_Keys_Draw`, vid_win.c `VID_MenuDraw`). Glyph
/// 12 is blank and 13 is the arrow, so the cursor is visible for a quarter
/// second out of every half second: a 4 Hz toggle on REAL time. (The menudot
/// spinner is the one menu animation on `host_time` — 10 Hz, see [`draw_menu`].)
pub fn menu_cursor_glyph(realtime: f64) -> u8 {
    OPTIONS_CURSOR_BASE + realtime_blink_bit(realtime, 4.0)
}

/// Which menu screen is showing. Mirrors the relevant `m_state` values from
/// menu.c (`m_main`, `m_singleplayer`, `m_load`, `m_save`, `m_multiplayer`,
/// `m_options`, `m_keys`, `m_video`, `m_help`, `m_quit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuScreen {
    /// The top-level menu (`m_main`): Single Player / Multiplayer / Options /
    /// Help / Quit.
    Main,
    /// The single-player submenu (`m_singleplayer`): New Game / Load / Save.
    SinglePlayer,
    /// The load-game slot list (`m_load`): [`MAX_SAVEGAMES`] rows showing the
    /// host-set slot comments (empty = [`UNUSED_SLOT`]). Enter on a loadable
    /// slot emits [`MenuAction::LoadSlot`]; on an unused slot it does nothing —
    /// the C's `M_Load_Key` returns when `!loadable[cursor]`.
    Load,
    /// The save-game slot list (`m_save`): Enter emits [`MenuAction::SaveSlot`]
    /// for the highlighted slot (`M_Save_Key`). Opening it is refused while no
    /// local game is running (`M_Menu_Save_f`'s `!sv.active` check, mapped to
    /// [`Menu::set_game_active`]).
    Save,
    /// The multiplayer submenu (`m_multiplayer`): Join / New Game / Setup over
    /// the `mp_menu` art. Netcode is out of scope, so — like the C with zero
    /// net drivers — Join/New Game don't respond and the screen shows
    /// "No Communications Available" (plus a port-scope note line).
    Multiplayer,
    /// The options submenu (`m_options`): the full 13-row layout
    /// ([`OPTIONS_ITEMS`]). Sliders + checkboxes are adjusted with left/right.
    Options,
    /// The Customize-controls screen (`m_keys`): the [`BINDNAMES`] list with a
    /// cursor; Enter grabs the next key to rebind (`bind_grab`), Backspace/Del
    /// unbind (`M_Keys_Key`).
    Keys,
    /// The video-modes screen (`m_video`): this port's mode list is
    /// [`RESOLUTION_PRESETS`]; cursor + Enter applies a mode
    /// ([`MenuAction::ResolutionChanged`]), like `VID_MenuKey`'s K_ENTER
    /// `VID_SetMode` (vid_win.c).
    Video,
    /// The Help/Ordering screen (`m_help`): pages through
    /// `gfx/help0.lmp`..`help5.lmp` with left/right ([`NUM_HELP_PAGES`] pages).
    Help,
    /// The Quit confirmation prompt (`m_quit`): "Are you sure you want to quit?".
    Quit,
}

impl MenuScreen {
    /// The number of selectable items on this screen (the cursor wraps within it).
    /// Help/Quit have no cursor list (1 item — the screen itself) so up/down are
    /// inert there; Help pages with left/right, Quit answers Y/N.
    fn item_count(self) -> usize {
        match self {
            MenuScreen::Main => MAIN_ITEMS,
            MenuScreen::SinglePlayer => SINGLEPLAYER_ITEMS,
            MenuScreen::Load | MenuScreen::Save => MAX_SAVEGAMES,
            MenuScreen::Multiplayer => MULTIPLAYER_ITEMS,
            MenuScreen::Options => OPTIONS_ITEMS,
            MenuScreen::Keys => NUM_BINDNAMES,
            MenuScreen::Video => RESOLUTION_PRESETS.len(),
            MenuScreen::Help | MenuScreen::Quit => 1,
        }
    }
}

/// One queued `S_LocalSound` from the menu (menu.c). The host drains these via
/// [`Menu::take_sounds`] and plays each like the C's `S_LocalSound` — a
/// view-entity sound at full volume, centred, no distance falloff
/// (`S_StartSound(cl.viewentity, -1, sfx, vec3_origin, 1, 1)`, snd_dma.c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuSound {
    /// `misc/menu1.wav` — cursor movement (and the keys-menu bind grab,
    /// `M_Keys_Key`; and every `VID_MenuKey` press).
    Menu1,
    /// `misc/menu2.wav` — `m_entersound`: entering a screen / Enter select.
    /// (The C latches the flag and plays it on the next `M_Draw` so pic caching
    /// can't stutter the sample; with pre-decoded Web Audio buffers that delay
    /// is unnecessary, so this port queues it at the trigger — EXCEPT where the
    /// menu closes before the next draw, where the C's latch never fires and we
    /// queue nothing.)
    Menu2,
    /// `misc/menu3.wav` — `M_AdjustSliders` (any Options left/right/Enter-adjust).
    Menu3,
}

impl MenuSound {
    /// The sample path relative to `sound/` (the form QuakeC sample names take;
    /// `S_LocalSound` passes exactly these strings).
    pub fn sample(self) -> &'static str {
        match self {
            MenuSound::Menu1 => "misc/menu1.wav",
            MenuSound::Menu2 => "misc/menu2.wav",
            MenuSound::Menu3 => "misc/menu3.wav",
        }
    }
}

/// The most local-sounds the menu queues between host drains: a bound on
/// [`Menu::take_sounds`]'s backlog so spamming menu keys without a running
/// `step` loop can't grow the queue without bound.
const MENU_SOUND_CAP: usize = 16;

/// What pressing Enter (or the menu closing) asks the host to do. The wasm/tool
/// front-end turns these into engine actions (e.g. [`MenuAction::NewGame`]
/// rebuilds the walk on [`NEW_GAME_MAP`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    /// Nothing to do (the selection only changed the screen, or — like the C —
    /// the item doesn't respond: Load on an unused slot, Multiplayer Join with
    /// no net drivers).
    None,
    /// Enter on a loadable Load slot (`M_Load_Key` K_ENTER: `load sN`). The
    /// menu already closed (`m_state = m_none; key_dest = key_game`); the host
    /// dispatches the actual load.
    LoadSlot(usize),
    /// Enter on a Save slot (`M_Save_Key` K_ENTER: `save sN`). The menu already
    /// closed; the host dispatches the actual save.
    SaveSlot(usize),
    /// Start a fresh single-player game on [`NEW_GAME_MAP`] and close the menu.
    NewGame,
    /// Backed out of a submenu to the main screen (Escape on a submenu).
    Back,
    /// The menu just closed (Escape on the main screen, or confirmed Quit).
    Closed,
    /// Options "Go to console": the host should close the menu and open the
    /// drop-down console (`m_state = m_none; Con_ToggleConsole_f()`).
    OpenConsole,
    /// Options "Reset to defaults": the host should reset the option cvars
    /// (`exec default.cfg`). [`Menu::select`] already reset the in-menu values
    /// (viewsize, gamma, volume, sensitivity, bindings, ...); the host reads them
    /// live each frame. The video mode is not a default.cfg cvar and stays.
    ResetDefaults,
    /// Enter on a Video Options mode line (`VID_MenuKey` K_ENTER -> `VID_SetMode`):
    /// the host must reallocate its framebuffer to [`Menu::resolution`].
    ResolutionChanged,
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
    /// Index into [`RESOLUTION_PRESETS`] of the live video mode (`vid_modenum`):
    /// what the Video Options list marks as current and opens its cursor on.
    /// The host keeps it synced to the real framebuffer
    /// ([`sync_resolution`](Menu::sync_resolution)); Enter on the Video list sets it.
    res_preset: usize,
    /// `viewsize` cvar (`scr_viewsize`), [`VIEWSIZE_MIN`]..=[`VIEWSIZE_MAX`]:
    /// the host frames the 3-D view with it ([`calc_refdef`]).
    viewsize: f32,
    /// `sensitivity` cvar (Mouse Speed), [`SENS_MIN`]..=[`SENS_MAX`].
    sensitivity: f32,
    /// `volume` cvar (Sound Volume), [`VOLUME_MIN`]..=[`VOLUME_MAX`]. The host maps
    /// it to a 0.0..=1.0 master gain.
    volume: f32,
    /// `v_gamma` cvar (Brightness), [`GAMMA_MIN`]..=[`GAMMA_MAX`]. LIVE: the
    /// host runs the presented frame through [`build_gamma_table`] with this
    /// (a byte-exact identity at the default 1.0).
    gamma: f32,
    /// `bgmvolume` cvar (CD Music Volume), [`BGM_MIN`]..=[`BGM_MAX`]. Live cvar;
    /// no CD audio exists to play at it (see the [`BGM_DEFAULT`] DEVIATION note).
    bgm_volume: f32,
    /// `cl_forwardspeed > 200` (Always Run). LIVE: the host swaps
    /// cl_forwardspeed/cl_backspeed 200 <-> 400 on it (M_AdjustSliders case 8).
    /// DEVIATION: defaults ON in this port (id's default.cfg leaves
    /// cl_forwardspeed at 200, i.e. off) — nobody wants to walk.
    always_run: bool,
    /// `m_pitch < 0` (Invert Mouse). LIVE: the host flips the mouse-pitch sign
    /// (in_win.c IN_MouseMove: `cl.viewangles[PITCH] += m_pitch.value * mouse_y`).
    invert_mouse: bool,
    /// `lookspring` cvar. LIVE: the C re-centres pitch when `+mlook` releases
    /// (`IN_MLookUp` -> `V_StartPitchDrift`, cl_input.c); this port's mouse-look
    /// is permanent while the pointer is locked, so the host maps the mlook
    /// RELEASE onto pointer-unlock (leaving pointer lock re-centres pitch).
    lookspring: bool,
    /// `lookstrafe` cvar. LIVE: while mouse-looking (always, under pointer
    /// lock), mouse X becomes strafe instead of yaw (in_win.c IN_MouseMove).
    lookstrafe: bool,
    /// The current Help page (`help_page`, `0..NUM_HELP_PAGES`).
    help_page: usize,
    /// Which screen the Quit prompt was raised from, restored on "No"
    /// (`m_quit_prevstate` / `wasInMenus`).
    quit_prev: MenuScreen,
    /// The Keys screen is waiting for the next key to bind (`bind_grab`,
    /// `M_Keys_Key`). The host routes raw keys to [`Menu::bind_key`] while set.
    bind_grab: bool,
    /// `keybindings[256]` (keys.c), as keynum -> [`BINDNAMES`] index. The menu
    /// owns the table; the host queries [`Menu::action_for_key`] to drive input.
    bindings: [Option<u8>; 256],
    /// Queued `S_LocalSound`s (menu1/menu2/menu3), drained by
    /// [`Menu::take_sounds`]. Capped at [`MENU_SOUND_CAP`].
    sounds: Vec<MenuSound>,
    /// The Load/Save slot comments (`m_filenames` from `M_ScanSaves`), host-set
    /// via [`Menu::set_save_comments`]. An empty string = unused slot (draws
    /// [`UNUSED_SLOT`], not loadable). All empty until a savegame engine fills
    /// them.
    save_comments: [String; MAX_SAVEGAMES],
    /// Whether a local single-player game is running (`sv.active &&
    /// !cl.intermission && svs.maxclients == 1`, the `M_Menu_Save_f` gate). The
    /// host keeps it current via [`Menu::set_game_active`]; while false the
    /// Save screen refuses to open.
    game_active: bool,
}

impl Default for Menu {
    fn default() -> Self {
        Menu::new()
    }
}

impl Menu {
    /// A closed menu sitting on the main screen with the cursor on the first item,
    /// with the Options cvars at their id defaults — except Always Run, which
    /// this port defaults ON (see the field's DEVIATION note).
    pub fn new() -> Menu {
        Menu {
            visible: false,
            screen: MenuScreen::Main,
            cursor: 0,
            res_preset: 0,
            viewsize: VIEWSIZE_DEFAULT,
            sensitivity: SENS_DEFAULT,
            volume: VOLUME_DEFAULT,
            gamma: GAMMA_DEFAULT,
            bgm_volume: BGM_DEFAULT,
            always_run: true,
            invert_mouse: false,
            lookspring: false,
            lookstrafe: false,
            help_page: 0,
            quit_prev: MenuScreen::Main,
            bind_grab: false,
            bindings: default_bindings(),
            sounds: Vec::new(),
            save_comments: Default::default(),
            game_active: false,
        }
    }

    /// Queue one `S_LocalSound` for the host to drain (bounded; a host that
    /// never drains can't leak).
    fn snd(&mut self, s: MenuSound) {
        if self.sounds.len() < MENU_SOUND_CAP {
            self.sounds.push(s);
        }
    }

    /// Drain the queued menu local-sounds (menu1/menu2/menu3), in fire order.
    /// The host plays each per `S_LocalSound` semantics (full volume, centred,
    /// no attenuation — see [`MenuSound`]).
    pub fn take_sounds(&mut self) -> Vec<MenuSound> {
        std::mem::take(&mut self.sounds)
    }

    /// Tell the menu whether a local single-player game is running — the
    /// `M_Menu_Save_f` gate (`!sv.active || cl.intermission || svs.maxclients
    /// != 1` all refuse). The host refreshes this every frame; Save refuses to
    /// open while false.
    pub fn set_game_active(&mut self, active: bool) {
        self.game_active = active;
    }

    /// Set the 12 Load/Save slot comments (`M_ScanSaves`' `m_filenames`): the
    /// host's savegame engine fills these from the `sN.sav` headers; an empty
    /// string marks the slot unused ([`UNUSED_SLOT`] is drawn, Enter on Load
    /// refuses it).
    pub fn set_save_comments(&mut self, comments: [String; MAX_SAVEGAMES]) {
        self.save_comments = comments;
    }

    /// Set one slot's comment (the host refreshes slots individually as the
    /// page reads each stored savegame out of localStorage). Out-of-range is
    /// ignored; an empty string marks the slot unused.
    pub fn set_save_comment(&mut self, i: usize, comment: String) {
        if let Some(c) = self.save_comments.get_mut(i) {
            *c = comment;
        }
    }

    /// The comment for save slot `i` (empty = unused). Out-of-range is empty.
    pub fn save_comment(&self, i: usize) -> &str {
        self.save_comments.get(i).map(String::as_str).unwrap_or("")
    }

    /// Whether Load may act on slot `i` (`loadable[i]` in `M_ScanSaves`): a
    /// non-empty host-set comment means a real save exists there.
    pub fn slot_loadable(&self, i: usize) -> bool {
        !self.save_comment(i).is_empty()
    }

    /// The screen currently displayed.
    pub fn screen(&self) -> MenuScreen {
        self.screen
    }

    /// The highlighted item index on the current screen.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The current Help page index (`0..NUM_HELP_PAGES`).
    pub fn help_page(&self) -> usize {
        self.help_page
    }

    /// Open the menu on the main screen (`M_Menu_Main_f`): show it and reset to the
    /// top-level screen with the cursor on the first item. Plays the enter sound
    /// (`m_entersound = true` in the C).
    pub fn open(&mut self) {
        self.visible = true;
        self.screen = MenuScreen::Main;
        self.cursor = 0;
        self.bind_grab = false;
        self.snd(MenuSound::Menu2);
    }

    /// Close the menu (`key_dest = key_game`). Leaves the screen/cursor as they
    /// were so a later `open` resets them.
    pub fn close(&mut self) {
        self.visible = false;
    }

    /// Reset the menu's NAVIGATION to boot state — closed, on the Main screen,
    /// cursor on the first item, no Help page / Quit return / bind grab, queued
    /// sounds dropped — while KEEPING every user choice: the Options cvars
    /// (Screen size, gamma, sensitivity, volume, CD volume, Always Run, Invert
    /// Mouse, lookspring, lookstrafe) and the whole key-bindings table. In
    /// WinQuake a map start / New Game only restarts the server: cvars and
    /// `keybindings[]` live in host state (persisted by
    /// `Host_WriteConfiguration`) and are never reset by `map start`
    /// (`M_SinglePlayer_Key`). The host calls this at every re-boot site that
    /// used to rebuild the Menu wholesale, so "rebind keys, set Always Run,
    /// then New Game" keeps the player's setup. The host-mirrored externals —
    /// Load/Save slot comments (`set_save_comments`) and the game-active gate
    /// (refreshed every `step`) — survive too: they reflect engine state, not
    /// navigation.
    pub fn reset_nav(&mut self) {
        self.visible = false;
        self.screen = MenuScreen::Main;
        self.cursor = 0;
        self.help_page = 0;
        self.quit_prev = MenuScreen::Main;
        self.bind_grab = false;
        self.sounds.clear();
    }

    /// Open the menu directly on the Help/Ordering screen (`M_Menu_Help_f`,
    /// menu.c): what the `help` console command — and therefore the shareware
    /// `svc_sellscreen` at episode end — runs. Resets to the first page
    /// (`help_page = 0`) like the C, and plays the enter sound (`m_entersound`).
    pub fn open_help(&mut self) {
        self.visible = true;
        self.screen = MenuScreen::Help;
        self.help_page = 0;
        self.cursor = 0;
        self.bind_grab = false;
        self.snd(MenuSound::Menu2);
    }

    /// Toggle the menu (`M_ToggleMenu_f`): if hidden, open on the main screen; if
    /// showing a submenu, go back to main; if already on the main screen, close.
    /// Returns the resulting [`MenuAction`] (`Closed` when it closed, else `None`).
    /// Opening / backing to main plays `m_entersound` (menu2); closing queues
    /// nothing audible (the C latches the flag but `M_Draw` never runs to fire
    /// it, and the next open re-latches it anyway).
    pub fn toggle(&mut self) -> MenuAction {
        if !self.visible {
            self.open();
            MenuAction::None
        } else if self.screen != MenuScreen::Main {
            // M_ToggleMenu_f -> M_Menu_Main_f (m_entersound = true).
            self.screen = MenuScreen::Main;
            self.cursor = 0;
            self.bind_grab = false;
            self.snd(MenuSound::Menu2);
            MenuAction::Back
        } else {
            self.close();
            MenuAction::Closed
        }
    }

    /// Move the cursor by `delta` (down = +1, up = -1), wrapping within the current
    /// screen's item count — exactly the `++/--` wrap in `M_Main_Key` /
    /// `M_Options_Key`. `delta` may be any magnitude; it wraps modulo the item
    /// count. On the Help screen up/down also page (the C maps `K_UPARROW`/
    /// `K_DOWNARROW` to page +/-); see [`page`](Menu::page).
    pub fn move_cursor(&mut self, delta: i32) {
        if self.screen == MenuScreen::Help {
            // M_Help_Key: UP = next page (m_help_page++), DOWN = previous. The host
            // passes up = -1 / down = +1 (cursor convention), so negate to map up
            // onto +1 (next). Previously up went backwards. (Paging latches
            // m_entersound in the C; `page` queues the menu2.)
            self.page(-delta.signum());
            return;
        }
        if self.screen == MenuScreen::Quit {
            return; // M_Quit_Key: up/down fall through to `default: break`.
        }
        let n = self.screen.item_count();
        if n == 0 {
            self.cursor = 0;
            return;
        }
        // Every M_*_Key cursor move plays misc/menu1.wav.
        self.snd(MenuSound::Menu1);
        let n_i = n as i32;
        // Wrap into 0..n even for large / negative deltas.
        let next = (self.cursor as i32 + delta).rem_euclid(n_i);
        self.cursor = next as usize;
    }

    /// Activate the highlighted item (Enter / `K_ENTER`).
    ///
    /// * Main > Single Player / Multiplayer / Options / Help: switch screen,
    ///   cursor reset ([`MenuAction::None`]).
    /// * Main > Quit: raise the Quit confirm prompt ([`MenuAction::None`]).
    /// * SinglePlayer > New Game: [`MenuAction::NewGame`] and close the menu.
    /// * SinglePlayer > Load / Save: open the slot lists (`M_Menu_Load_f` /
    ///   `M_Menu_Save_f`; Save refuses while no game runs).
    /// * Load > slot: [`MenuAction::LoadSlot`] + close when loadable, else
    ///   nothing (`M_Load_Key`'s `!loadable` return).
    /// * Save > slot: [`MenuAction::SaveSlot`] + close (`M_Save_Key`).
    /// * Multiplayer > Join/New Game: no net drivers, nothing (like the C);
    ///   Setup: not ported ([`MenuAction::None`]).
    /// * Options > Customize controls: the Keys screen; Video Options: the
    ///   video-mode list.
    /// * Options > Go to console: [`MenuAction::OpenConsole`].
    /// * Options > Reset to defaults: reset the in-menu cvars, return
    ///   [`MenuAction::ResetDefaults`].
    /// * Options analog/checkbox rows: Enter nudges them right (the C falls through
    ///   to `M_AdjustSliders(1)`).
    /// * Keys > row: start the bind grab (`bind_grab`), unbinding first when the
    ///   row already shows two keys (`M_Keys_Key` K_ENTER).
    /// * Video > row: apply the highlighted preset ([`MenuAction::ResolutionChanged`]).
    /// * Quit > Enter == "Yes": close the menu ([`MenuAction::Closed`]).
    /// * Help: Enter is inert ([`MenuAction::None`]).
    pub fn select(&mut self) -> MenuAction {
        match self.screen {
            MenuScreen::Main => {
                // M_Main_Key K_ENTER: m_entersound = true for every item.
                self.snd(MenuSound::Menu2);
                match self.cursor {
                    0 => {
                        // M_Menu_SinglePlayer_f
                        self.screen = MenuScreen::SinglePlayer;
                        self.cursor = 0;
                        MenuAction::None
                    }
                    1 => {
                        // M_Menu_MultiPlayer_f
                        self.screen = MenuScreen::Multiplayer;
                        self.cursor = 0;
                        MenuAction::None
                    }
                    2 => {
                        // M_Menu_Options_f
                        self.screen = MenuScreen::Options;
                        self.cursor = 0;
                        MenuAction::None
                    }
                    3 => {
                        // M_Menu_Help_f
                        self.screen = MenuScreen::Help;
                        self.cursor = 0;
                        self.help_page = 0;
                        MenuAction::None
                    }
                    4 => {
                        // M_Menu_Quit_f: pop the confirm prompt (does NOT quit yet).
                        self.open_quit();
                        MenuAction::None
                    }
                    _ => MenuAction::None,
                }
            }
            MenuScreen::SinglePlayer => match self.cursor {
                0 => {
                    // New Game: the C runs `map start`; we start the hub and
                    // close. (M_SinglePlayer_Key latches m_entersound, but the
                    // menu closes before M_Draw can fire it — silent.)
                    self.close();
                    self.screen = MenuScreen::Main;
                    self.cursor = 0;
                    MenuAction::NewGame
                }
                1 => {
                    // M_Menu_Load_f (M_ScanSaves already ran host-side: the
                    // slot comments are whatever set_save_comments put there).
                    self.snd(MenuSound::Menu2);
                    self.screen = MenuScreen::Load;
                    self.cursor = 0;
                    MenuAction::None
                }
                2 => {
                    // M_Menu_Save_f: refuse without an active local game
                    // (!sv.active / cl.intermission / maxclients != 1 — the
                    // host folds those into game_active). The C latches
                    // m_entersound BEFORE the early return and the menu keeps
                    // drawing, so the menu2 still plays either way.
                    self.snd(MenuSound::Menu2);
                    if self.game_active {
                        self.screen = MenuScreen::Save;
                        self.cursor = 0;
                    }
                    MenuAction::None
                }
                _ => MenuAction::None,
            },
            MenuScreen::Load => {
                // M_Load_Key K_ENTER: menu2 first, then return unless loadable.
                self.snd(MenuSound::Menu2);
                if !self.slot_loadable(self.cursor) {
                    return MenuAction::None;
                }
                // m_state = m_none; key_dest = key_game; Cbuf "load sN".
                let slot = self.cursor;
                self.close();
                self.screen = MenuScreen::Main;
                self.cursor = 0;
                MenuAction::LoadSlot(slot)
            }
            MenuScreen::Save => {
                // M_Save_Key K_ENTER (no sound in the C): m_state = m_none;
                // key_dest = key_game; Cbuf "save sN".
                let slot = self.cursor;
                self.close();
                self.screen = MenuScreen::Main;
                self.cursor = 0;
                MenuAction::SaveSlot(slot)
            }
            MenuScreen::Multiplayer => {
                // M_MultiPlayer_Key K_ENTER: m_entersound = true; items 0/1
                // only open the net menu when a driver is available (none here,
                // like a C build with no network) and item 2 (Setup) is not
                // ported — so every item responds with the sound alone.
                self.snd(MenuSound::Menu2);
                MenuAction::None
            }
            MenuScreen::Options => match self.cursor {
                ROW_CONTROLS => {
                    // M_Menu_Keys_f
                    self.snd(MenuSound::Menu2);
                    self.screen = MenuScreen::Keys;
                    self.cursor = 0;
                    self.bind_grab = false;
                    MenuAction::None
                }
                ROW_VIDEO => {
                    // M_Menu_Video_f: open the mode list with the cursor on the
                    // current mode (vid_win.c keeps vid_line on the live mode).
                    self.snd(MenuSound::Menu2);
                    self.screen = MenuScreen::Video;
                    self.cursor = self.res_preset.min(RESOLUTION_PRESETS.len() - 1);
                    MenuAction::None
                }
                ROW_CONSOLE => {
                    // m_state = m_none; Con_ToggleConsole_f(). The latched
                    // m_entersound never fires (the menu closed) — silent.
                    self.close();
                    MenuAction::OpenConsole
                }
                ROW_DEFAULTS => {
                    // Cbuf_AddText("exec default.cfg"): reset every option cvar
                    // (m_entersound plays — the menu stays up).
                    self.snd(MenuSound::Menu2);
                    self.reset_defaults();
                    MenuAction::ResetDefaults
                }
                // Every other row: Enter latches m_entersound AND falls through
                // to M_AdjustSliders(1) (its own menu3) — the C audibly plays
                // BOTH. (Screen size is viewsize: the host reads it each frame.)
                _ => {
                    self.snd(MenuSound::Menu2);
                    self.adjust(1);
                    MenuAction::None
                }
            },
            MenuScreen::Keys => {
                // M_Keys_Key K_ENTER: menu2; unbind first when the row already
                // shows two keys, then grab the next key.
                self.snd(MenuSound::Menu2);
                let keys = self.find_keys_for_command(self.cursor);
                if keys[1].is_some() {
                    self.unbind_command(self.cursor);
                }
                self.bind_grab = true;
                MenuAction::None
            }
            MenuScreen::Video => {
                // VID_MenuKey K_ENTER: menu1 (NOT menu2) + VID_SetMode on the
                // highlighted mode line.
                self.snd(MenuSound::Menu1);
                self.res_preset = self.cursor.min(RESOLUTION_PRESETS.len() - 1);
                MenuAction::ResolutionChanged
            }
            MenuScreen::Help => MenuAction::None,
            MenuScreen::Quit => {
                // Enter == "Yes": Host_Quit_f. Here that closes the menu (quit to
                // the attract loop). (The C's M_Quit_Key ignores Enter — only
                // y/Y quits — but this port has always accepted Enter as Yes.)
                self.close();
                self.screen = MenuScreen::Main;
                self.cursor = 0;
                MenuAction::Closed
            }
        }
    }

    /// Back out (Escape / `K_ESCAPE`). A hidden menu is a no-op
    /// ([`MenuAction::None`]). Otherwise:
    /// * a Keys bind-grab in progress is cancelled (`M_Keys_Key`, the grab
    ///   branch's `K_ESCAPE`) — the screen stays;
    /// * SinglePlayer/Multiplayer/Options/Help return to Main; Load/Save return
    ///   to SinglePlayer; Keys/Video return to Options (each `M_Menu_*_f` plays
    ///   `m_entersound`) — all [`MenuAction::Back`];
    /// * the Quit prompt answers "No" → restores the previous screen
    ///   ([`MenuAction::Back`]);
    /// * the Main screen closes the menu ([`MenuAction::Closed`]).
    pub fn cancel(&mut self) -> MenuAction {
        if !self.visible {
            return MenuAction::None;
        }
        if self.bind_grab {
            // M_Keys_Key while defining a key: menu1; Escape just ends the grab.
            self.snd(MenuSound::Menu1);
            self.bind_grab = false;
            return MenuAction::None;
        }
        match self.screen {
            MenuScreen::SinglePlayer
            | MenuScreen::Multiplayer
            | MenuScreen::Options
            | MenuScreen::Help => {
                // M_*_Key K_ESCAPE -> M_Menu_Main_f (m_entersound = true).
                self.screen = MenuScreen::Main;
                self.cursor = 0;
                self.snd(MenuSound::Menu2);
                MenuAction::Back
            }
            MenuScreen::Load | MenuScreen::Save => {
                // M_Load_Key / M_Save_Key K_ESCAPE -> M_Menu_SinglePlayer_f.
                self.screen = MenuScreen::SinglePlayer;
                self.cursor = 0;
                self.snd(MenuSound::Menu2);
                MenuAction::Back
            }
            MenuScreen::Keys => {
                // M_Keys_Key K_ESCAPE -> M_Menu_Options_f (m_entersound).
                self.screen = MenuScreen::Options;
                self.cursor = 0;
                self.snd(MenuSound::Menu2);
                MenuAction::Back
            }
            MenuScreen::Video => {
                // VID_MenuKey K_ESCAPE: menu1, then M_Menu_Options_f (menu2).
                self.snd(MenuSound::Menu1);
                self.screen = MenuScreen::Options;
                self.cursor = 0;
                self.snd(MenuSound::Menu2);
                MenuAction::Back
            }
            MenuScreen::Quit => {
                // M_Quit_Key 'n'/Escape: restore the screen the prompt rose from
                // (wasInMenus -> m_entersound = true).
                self.screen = self.quit_prev;
                self.snd(MenuSound::Menu2);
                MenuAction::Back
            }
            MenuScreen::Main => {
                // M_Main_Key K_ESCAPE -> key_dest = key_game
                self.close();
                MenuAction::Closed
            }
        }
    }

    /// Raise the Quit confirmation prompt (`M_Menu_Quit_f`): remember the screen we
    /// came from (so "No" restores it) and switch to [`MenuScreen::Quit`]. Visible
    /// either way (the prompt is reachable from the game via `menu_cancel`-open too).
    pub fn open_quit(&mut self) {
        if self.screen == MenuScreen::Quit {
            return;
        }
        self.quit_prev = self.screen;
        self.visible = true;
        self.screen = MenuScreen::Quit;
    }

    /// Answer the Quit prompt "Yes" (the literal `Y` key) — quit: close the menu.
    /// A no-op off the Quit screen. Returns [`MenuAction::Closed`] when it quit,
    /// else [`MenuAction::None`].
    pub fn quit_yes(&mut self) -> MenuAction {
        if self.screen != MenuScreen::Quit {
            return MenuAction::None;
        }
        self.close();
        self.screen = MenuScreen::Main;
        self.cursor = 0;
        MenuAction::Closed
    }

    /// Answer the Quit prompt "No" (the literal `N` key) — back out, same as
    /// [`cancel`](Menu::cancel) on the Quit screen. A no-op off the Quit screen.
    pub fn quit_no(&mut self) -> MenuAction {
        if self.screen != MenuScreen::Quit {
            return MenuAction::None;
        }
        // M_Quit_Key 'n': wasInMenus -> m_entersound = true.
        self.screen = self.quit_prev;
        self.snd(MenuSound::Menu2);
        MenuAction::Back
    }

    /// Page the Help screen by `dir` (right/up = +1 next, left/down = -1 previous),
    /// wrapping over [`NUM_HELP_PAGES`] (`M_Help_Key`, which latches
    /// `m_entersound` — menu2 — on every page turn). A no-op off the Help screen.
    pub fn page(&mut self, dir: i32) {
        if self.screen != MenuScreen::Help {
            return;
        }
        self.snd(MenuSound::Menu2);
        self.help_page = help_page_wrap(self.help_page as i32 + dir.signum());
    }

    /// Adjust the highlighted Options row by `delta` (left = -1, right = +1),
    /// porting `M_AdjustSliders` — plus the screens whose `M_*_Key` maps
    /// left/right onto cursor movement (`M_Load_Key`/`M_Save_Key`/`M_Keys_Key`
    /// pair LEFT with UP and RIGHT with DOWN; `VID_MenuKey` steps the mode line)
    /// and Help paging. Nothing here changes the video mode: that is Enter on
    /// the Video Options list ([`MenuAction::ResolutionChanged`]).
    pub fn adjust(&mut self, delta: i32) {
        let step = delta.signum();
        if step == 0 {
            return;
        }
        // M_Help_Key: RIGHT = next page, LEFT = previous page (the C handles
        // left/right on Help identically to up/down).
        if self.screen == MenuScreen::Help {
            self.page(step);
            return;
        }
        // M_Load_Key / M_Save_Key / M_Keys_Key: LEFT pairs with UP and RIGHT
        // with DOWN (cursor movement, menu1 inside move_cursor). VID_MenuKey
        // also moves the mode line on left/right (single-column here).
        if matches!(
            self.screen,
            MenuScreen::Load | MenuScreen::Save | MenuScreen::Keys | MenuScreen::Video
        ) {
            self.move_cursor(step);
            return;
        }
        if self.screen != MenuScreen::Options {
            return;
        }
        // M_AdjustSliders plays misc/menu3.wav unconditionally — even when the
        // cursor sits on an action row the switch below ignores.
        self.snd(MenuSound::Menu3);
        let d = step as f32;
        match self.cursor {
            ROW_SCREENSIZE => {
                // scr_viewsize.value += dir * 10, clamped 30..=120.
                self.viewsize =
                    (self.viewsize + d * VIEWSIZE_STEP).clamp(VIEWSIZE_MIN, VIEWSIZE_MAX);
            }
            ROW_BRIGHTNESS => {
                // v_gamma.value -= dir * 0.05 (LEFT brightens), clamp 0.5..=1.
                self.gamma = (self.gamma - d * GAMMA_STEP).clamp(GAMMA_MIN, GAMMA_MAX);
            }
            ROW_MOUSESPEED => {
                self.sensitivity =
                    (self.sensitivity + d * SENS_STEP).clamp(SENS_MIN, SENS_MAX);
            }
            ROW_CDVOLUME => {
                self.bgm_volume = (self.bgm_volume + d * BGM_STEP).clamp(BGM_MIN, BGM_MAX);
            }
            ROW_SNDVOLUME => {
                self.volume = (self.volume + d * VOLUME_STEP).clamp(VOLUME_MIN, VOLUME_MAX);
            }
            // Checkboxes ignore the direction and simply toggle (matches the C,
            // which flips the bool regardless of `dir`).
            ROW_ALWAYSRUN => self.always_run = !self.always_run,
            ROW_INVERTMOUSE => self.invert_mouse = !self.invert_mouse,
            ROW_LOOKSPRING => self.lookspring = !self.lookspring,
            ROW_LOOKSTRAFE => self.lookstrafe = !self.lookstrafe,
            // Action rows (Customize / Console / Defaults / Video): not adjustable.
            _ => {}
        }
    }

    /// Reset every Options cvar to its *port* default (`exec default.cfg`) — id's
    /// values everywhere except Always Run, which resets to ON (this port's
    /// default; see the field's DEVIATION note). `default.cfg` sets `viewsize
    /// 100`. The video mode is not in `default.cfg`, so the live resolution
    /// stays. The key bindings reset too — the C's `default.cfg` is mostly
    /// `bind` lines, re-executed wholesale by this row.
    pub fn reset_defaults(&mut self) {
        self.viewsize = VIEWSIZE_DEFAULT;
        self.sensitivity = SENS_DEFAULT;
        self.volume = VOLUME_DEFAULT;
        self.gamma = GAMMA_DEFAULT;
        self.bgm_volume = BGM_DEFAULT;
        self.always_run = true;
        self.invert_mouse = false;
        self.lookspring = false;
        self.lookstrafe = false;
        self.bindings = default_bindings();
    }

    /// The current video mode `(width, height)` ([`RESOLUTION_PRESETS`] entry
    /// `res_preset`; `320x200` until the host syncs it). Enter on the Video
    /// Options list changes it and the host resizes its framebuffer to it.
    pub fn resolution(&self) -> (i32, i32) {
        RESOLUTION_PRESETS
            .get(self.res_preset)
            .copied()
            .unwrap_or(RESOLUTION_PRESETS[0])
    }

    /// Point the Video Options "current mode" at the preset matching `(w, h)`, if
    /// one exists (otherwise leave it). The host calls this with its *actual* render
    /// size so the displayed value always tracks reality — the framebuffer is the
    /// single source of truth, and the label can never desync from it (e.g. after
    /// a boot / New Game / `map` that changed the render size independently).
    pub fn sync_resolution(&mut self, w: i32, h: i32) {
        if let Some(i) = RESOLUTION_PRESETS.iter().position(|&(pw, ph)| pw == w && ph == h) {
            self.res_preset = i;
        }
    }

    /// The `viewsize` cvar (`scr_viewsize`, 30..=120, default 100): the host
    /// sizes the 3-D view and the status bar from it via [`calc_refdef`].
    pub fn viewsize(&self) -> f32 {
        self.viewsize
    }

    /// Set the `viewsize` cvar (the console's `viewsize <n>`), bounded to
    /// 30..=120 as SCR_CalcRefdef bounds it (and writes back) on the next frame.
    /// A non-number reads as 0 (`atof`), i.e. the minimum.
    pub fn set_viewsize(&mut self, v: f32) {
        let v = if v.is_finite() { v } else { 0.0 };
        self.viewsize = v.clamp(VIEWSIZE_MIN, VIEWSIZE_MAX);
    }

    /// `sizeup` (SCR_SizeUp_f): `viewsize += 10` (bounded as above). Bound to
    /// `+` and `=` in default.cfg.
    pub fn size_up(&mut self) {
        self.set_viewsize(self.viewsize + VIEWSIZE_STEP);
    }

    /// `sizedown` (SCR_SizeDown_f): `viewsize -= 10` (bounded as above). Bound
    /// to `-` in default.cfg.
    pub fn size_down(&mut self) {
        self.set_viewsize(self.viewsize - VIEWSIZE_STEP);
    }

    /// The Options "Mouse Speed" as a sensitivity multiplier the host applies to
    /// its baseline look sensitivity. id's `sensitivity` defaults to 3, so we
    /// normalise by [`SENS_DEFAULT`]: the out-of-the-box feel is unchanged (1.0x),
    /// and the 1..=11 range maps to a `0.33..=3.67` multiplier.
    pub fn mouse_sensitivity(&self) -> f32 {
        self.sensitivity / SENS_DEFAULT
    }

    /// The Options "Sound Volume" as a `0.0..=1.0` master gain (the `volume` cvar
    /// directly). Default 0.7.
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// The raw `sensitivity` cvar value (1..=11), for display/tests.
    pub fn sensitivity(&self) -> f32 {
        self.sensitivity
    }

    /// The `v_gamma` cvar (Brightness, 0.5..=1). The host runs the presented
    /// frame through [`build_gamma_table`] with this (identity at 1.0).
    pub fn gamma(&self) -> f32 {
        self.gamma
    }

    /// The `bgmvolume` cvar (CD Music Volume, 0..=1). Live cvar; no CD audio
    /// exists to play at it (see the [`BGM_DEFAULT`] DEVIATION note).
    pub fn bgm_volume(&self) -> f32 {
        self.bgm_volume
    }

    /// Whether the "Always Run" checkbox is on (`cl_forwardspeed > 200`): the
    /// host swaps cl_forwardspeed/cl_backspeed 200 <-> 400 on it.
    pub fn always_run(&self) -> bool {
        self.always_run
    }

    /// Whether the "Invert Mouse" checkbox is on (`m_pitch < 0`): the host
    /// flips the mouse-pitch sign.
    pub fn invert_mouse(&self) -> bool {
        self.invert_mouse
    }

    /// Whether the "Lookspring" checkbox is on: pitch re-centres when mouse-look
    /// disengages (pointer unlock in this port — see the field note).
    pub fn lookspring(&self) -> bool {
        self.lookspring
    }

    /// Whether the "Lookstrafe" checkbox is on: mouse X strafes instead of
    /// turning while mouse-looking.
    pub fn lookstrafe(&self) -> bool {
        self.lookstrafe
    }

    // --- key bindings (M_Keys_*, keys.c) -----------------------------------

    /// Whether the Keys screen is waiting for the next key to bind
    /// (`bind_grab`). While set the host routes RAW keys to
    /// [`bind_key`](Menu::bind_key) instead of menu navigation.
    pub fn bind_grabbing(&self) -> bool {
        self.bind_grab
    }

    /// Deliver the grabbed key (`M_Keys_Key`, the `bind_grab` branch): plays
    /// menu1; Escape cancels and the console key (backtick) is refused; any
    /// other key binds to the highlighted command. Either way the grab ends.
    /// A no-op when not grabbing.
    pub fn bind_key(&mut self, keynum: u8) {
        if !self.bind_grab {
            return;
        }
        self.snd(MenuSound::Menu1);
        if keynum != K_ESCAPE && keynum != b'`' {
            let cmd = self.cursor.min(NUM_BINDNAMES - 1);
            self.bindings[keynum as usize] = Some(cmd as u8);
        }
        self.bind_grab = false;
    }

    /// Backspace/Del on the Keys screen (`M_Keys_Key` K_BACKSPACE/K_DEL): plays
    /// menu2 and unbinds every key bound to the highlighted command. A no-op on
    /// any other screen (and while grabbing — the C's grab branch consumes the
    /// key as a BINDING first; the host routes it to [`bind_key`](Menu::bind_key)).
    pub fn keys_backspace(&mut self) {
        if self.screen != MenuScreen::Keys || self.bind_grab {
            return;
        }
        self.snd(MenuSound::Menu2);
        self.unbind_command(self.cursor.min(NUM_BINDNAMES - 1));
    }

    /// The [`BINDNAMES`] command index bound to `keynum`, if any — the host's
    /// per-keypress lookup (the inverse of the C consulting `keybindings[key]`
    /// in `Key_Event`).
    pub fn action_for_key(&self, keynum: u8) -> Option<usize> {
        self.bindings[keynum as usize].map(|c| c as usize)
    }

    /// `M_FindKeysForCommand` (menu.c): the first two keys bound to `cmd`, in
    /// keynum order (the C scans 0..256 ascending).
    pub fn find_keys_for_command(&self, cmd: usize) -> [Option<u8>; 2] {
        let mut out = [None; 2];
        let mut n = 0;
        for (k, b) in self.bindings.iter().enumerate() {
            if *b == Some(cmd as u8) {
                out[n] = Some(k as u8);
                n += 1;
                if n == 2 {
                    break;
                }
            }
        }
        out
    }

    /// `M_UnbindCommand` (menu.c): clear every key bound to `cmd`.
    pub fn unbind_command(&mut self, cmd: usize) {
        for b in self.bindings.iter_mut() {
            if *b == Some(cmd as u8) {
                *b = None;
            }
        }
    }
}

/// The slider knob's virtual-x offset, in pixels, from the trough's drawing
/// origin `x` (`M_DrawSlider`): the knob (glyph 131) sits at
/// `(SLIDER_RANGE-1)*8 * range`, with `range` clamped to `[0,1]`. So fraction 0
/// puts the knob on the left segment and fraction 1 on the rightmost of the
/// [`SLIDER_RANGE`] middle segments. A non-finite fraction is treated as 0.
fn slider_knob_offset(range: f32) -> f32 {
    let r = if range.is_finite() {
        range.clamp(0.0, 1.0)
    } else {
        0.0
    };
    (SLIDER_RANGE - 1) as f32 * 8.0 * r
}

/// The checkbox label text (`M_DrawCheckbox`): "on" / "off".
fn checkbox_text(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

/// Wrap a Help page index into `0..NUM_HELP_PAGES` (`M_Help_Key`: past the last
/// page wraps to 0, below 0 wraps to the last). Accepts any `i32`.
fn help_page_wrap(p: i32) -> usize {
    (p.rem_euclid(NUM_HELP_PAGES as i32)) as usize
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
    /// `gfx/p_option.lmp` — the "OPTIONS" title plaque (centered at y=4 on the
    /// options screen, like the other titles).
    pub p_option: Option<crate::wad::Qpic>,
    /// `gfx/p_load.lmp` — the "LOAD GAME" title (`M_Load_Draw`).
    pub p_load: Option<crate::wad::Qpic>,
    /// `gfx/p_save.lmp` — the "SAVE GAME" title (`M_Save_Draw`).
    pub p_save: Option<crate::wad::Qpic>,
    /// `gfx/p_multi.lmp` — the MULTIPLAYER title (`M_MultiPlayer_Draw`).
    pub p_multi: Option<crate::wad::Qpic>,
    /// `gfx/mp_menu.lmp` — the 3-item multiplayer list graphic (drawn at (72,32)).
    pub mp_menu: Option<crate::wad::Qpic>,
    /// `gfx/ttl_cstm.lmp` — the CUSTOMIZE CONTROLS title (`M_Keys_Draw`).
    pub ttl_cstm: Option<crate::wad::Qpic>,
    /// `gfx/vidmodes.lmp` — the VIDEO MODES title (vid_win.c `VID_MenuDraw`).
    pub vidmodes: Option<crate::wad::Qpic>,
    /// `gfx/menudot1.lmp`..`menudot6.lmp` — the 6-frame animated cursor.
    pub menudot: [Option<crate::wad::Qpic>; 6],
    /// `gfx/help0.lmp`..`help5.lmp` — the 6 full-screen Help/Ordering pages
    /// (`M_Help_Draw` blits the current one at (0,0)).
    pub help: [Option<crate::wad::Qpic>; NUM_HELP_PAGES],
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
// Mirrors Draw_Pic (draw.c); the C reads vid/draw globals passed explicitly here.
#[allow(clippy::too_many_arguments)]
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

/// Draw a `centerprint` message (SCR_DrawCenterString): each '\n'-split line is
/// centered horizontally in the 320x200 virtual screen and the block is centered
/// vertically, scaled to the framebuffer (`image.w/320`, the HUD/menu scale).
/// No-op without conchars or on an empty frame.
pub fn draw_centerprint(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    palette: &[[u8; 3]; 256],
    text: &str,
) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let scale = image.w as f32 / HUD_VIRT_W;
    let lines: Vec<&str> = text.split('\n').collect();
    // SCR_DrawCenterString: short messages (<= 4 lines) sit in the upper third at
    // y = vid.height*0.35 (200*0.35 = 70 in the virtual screen); taller blocks
    // start at y = 48 so they don't run off the bottom. NOT dead-centre.
    let mut vy = if lines.len() <= 4 { 200.0 * 0.35 } else { 48.0 };
    for line in lines {
        let w = line.len() as f32 * 8.0;
        let vx = ((HUD_VIRT_W - w) * 0.5).max(0.0);
        draw_string_scaled(image, conchars, vx, vy, line, scale, 0.0, 0.0, palette);
        vy += 8.0;
    }
}

/// Draw the notify lines (`bprint`/`sprint`, Con_DrawNotify): stacked at the
/// top-left of the 320x200 virtual screen, scaled to the framebuffer.
pub fn draw_notify(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    palette: &[[u8; 3]; 256],
    lines: &[&str],
) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let scale = image.w as f32 / HUD_VIRT_W;
    let mut vy = 8.0;
    for line in lines {
        draw_string_scaled(image, conchars, 8.0, vy, line, scale, 0.0, 0.0, palette);
        vy += 8.0;
    }
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

/// Draw ONE conchars glyph by its raw byte index (`M_DrawCharacter`), at virtual
/// `(vx, vy)` in 320x200 space, scaled+offset like [`draw_string_scaled`]. Unlike
/// `draw_string_scaled` (which speaks ASCII and skips byte 0 / space) this stamps
/// the exact cell `num`, so it can draw the slider strip (glyphs 128/129/130/131)
/// and the flashing cursor (glyphs 12/13). Palette-0 texels stay transparent; the
/// glyph cell falling outside the atlas is a silent skip.
#[allow(clippy::too_many_arguments)]
fn draw_char_scaled(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    vx: f32,
    vy: f32,
    num: u8,
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
    let cell_w = (conchars.width / 16).max(1) as usize;
    let cell_h = (conchars.height / 16).max(1) as usize;
    let cell_x = (num as usize % 16) * cell_w;
    let cell_y = (num as usize / 16) * cell_h;
    let block = scale.ceil().max(1.0) as i64;
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
            if texel == 0 {
                continue;
            }
            let px = (ox + (vx + gx as f32) * scale).floor() as i64;
            for by in 0..block {
                for bx in 0..block {
                    image.put((px + bx) as i32, (py + by) as i32, palette[texel as usize]);
                }
            }
        }
    }
}

/// Draw a slider widget (`M_DrawSlider`) with its trough origin at virtual
/// `(x, y)`: glyph 128 (left cap) at `x-8`, [`SLIDER_RANGE`] copies of glyph 129
/// (middle) starting at `x`, glyph 130 (right cap) just past them, and the knob
/// (glyph 131) at `x + slider_knob_offset(range)`. `range` is the cvar's [0,1]
/// fraction (clamped inside [`slider_knob_offset`]).
#[allow(clippy::too_many_arguments)]
fn draw_slider(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    x: f32,
    y: f32,
    range: f32,
    scale: f32,
    ox: f32,
    oy: f32,
    palette: &[[u8; 3]; 256],
) {
    draw_char_scaled(image, conchars, x - 8.0, y, SLIDER_LEFT_CHAR, scale, ox, oy, palette);
    for i in 0..SLIDER_RANGE {
        let cx = x + i as f32 * 8.0;
        draw_char_scaled(image, conchars, cx, y, SLIDER_MID_CHAR, scale, ox, oy, palette);
    }
    let right_x = x + SLIDER_RANGE as f32 * 8.0;
    draw_char_scaled(image, conchars, right_x, y, SLIDER_RIGHT_CHAR, scale, ox, oy, palette);
    let knob_x = x + slider_knob_offset(range);
    draw_char_scaled(image, conchars, knob_x, y, SLIDER_KNOB_CHAR, scale, ox, oy, palette);
}

/// Draw the main menu (or single-player submenu) over `image`, a port of
/// `M_Main_Draw` / `M_SinglePlayer_Draw`.
///
/// The layout is Quake's fixed 320x200 virtual canvas, scaled to fit `image`
/// (`scale = min(w/320, h/200)`) and centered, so it looks identical on the
/// 320x200 wasm framebuffer (scale 1, no offset) and on the 640x400 PPM the tool
/// writes (scale 2, centered).
///
/// Two clocks, exactly like the C: `host_time` (the clamped-frametime host
/// clock) drives the animated menudot spinner, `(int)(host_time*10) % 6`
/// (`M_Main_Draw` and friends); `realtime` (the unclamped wall clock) drives
/// every flashing conchars cursor, `12 + ((int)(realtime*4) & 1)` — see
/// [`menu_cursor_glyph`].
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
    host_time: f32,
    realtime: f64,
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
    let frame = if host_time.is_finite() && host_time > 0.0 {
        ((host_time * 10.0) as usize) % 6
    } else {
        0
    };
    // The flashing conchars cursor (Options / Load / Save / Keys / Video) runs
    // on REAL time at 4 Hz, independent of the menudot's host_time spinner.
    let cursor = menu_cursor_glyph(realtime);

    // The Help screen is a full-screen pic at (0,0); the Quit prompt is a small
    // text box; Load/Save/Keys/Video are a centered title + text rows with no
    // qplaque (M_Load_Draw etc. draw only the title pic). Dispatch them all
    // before drawing the plaque.
    match menu.screen {
        MenuScreen::Help => {
            draw_help_screen(image, menu, pics, scale, ox, oy, palette);
            return;
        }
        MenuScreen::Quit => {
            draw_quit_screen(image, conchars, scale, ox, oy, palette);
            return;
        }
        MenuScreen::Load | MenuScreen::Save => {
            draw_load_save_screen(image, menu, pics, conchars, scale, ox, oy, cursor, palette);
            return;
        }
        MenuScreen::Keys => {
            draw_keys_screen(image, menu, pics, conchars, scale, ox, oy, cursor, palette);
            return;
        }
        MenuScreen::Video => {
            draw_video_screen(image, menu, pics, conchars, scale, ox, oy, cursor, palette);
            return;
        }
        _ => {}
    }

    // The plaque is shared by the Main / SinglePlayer / Multiplayer / Options
    // screens (M_DrawTransPic (16,4)).
    if let Some(p) = &pics.qplaque {
        blit_qpic_at(image, p, 16.0, 4.0, scale, ox, oy, palette);
    }

    // The Options screen is laid out from text rows (it has no single list pic);
    // the Main / SinglePlayer screens use their pre-baked list graphic. Branch the
    // whole body so each screen draws its own title + rows.
    if menu.screen == MenuScreen::Options {
        draw_options_screen(image, menu, pics, conchars, scale, ox, oy, cursor, palette);
        return;
    }

    // M_MultiPlayer_Draw: the C's exact layout, plus the line a netless build
    // shows.
    if menu.screen == MenuScreen::Multiplayer {
        draw_multiplayer_screen(image, menu, pics, conchars, scale, ox, oy, frame, palette);
        return;
    }

    // The centered title + the item-list graphic differ per screen.
    let (title, list) = match menu.screen {
        MenuScreen::Main => (&pics.ttl_main, &pics.mainmenu),
        MenuScreen::SinglePlayer => (&pics.ttl_sgl, &pics.sp_menu),
        // Every other screen is handled above (early return); the catch-all keeps
        // the match exhaustive without a second layout here.
        _ => (&pics.p_option, &None),
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

/// The Options rows' vertical origin and step (`M_Options_Draw`: first label at
/// y=32, 8 px per row; the flashing cursor at `32 + cursor*8`).
const OPTIONS_ROW_Y0: f32 = 32.0;
const OPTIONS_ROW_STEP: f32 = 8.0;
/// The right-justified label column origin (`M_Print(16, ...)`). The labels are
/// pre-padded to a fixed width so their right edges line up at ~x=184, exactly as
/// id ships them (e.g. `"    Customize controls"`).
const OPTIONS_LABEL_X: f32 = 16.0;
/// Sliders + checkboxes draw at virtual x=220 (`M_DrawSlider(220,…)` /
/// `M_DrawCheckbox(220,…)`).
const OPTIONS_WIDGET_X: f32 = 220.0;

/// The 13 Options labels, pre-padded to right-justify at x≈184 — copied verbatim
/// from `M_Options_Draw` so the column lines up with the widgets at x=220.
const OPTIONS_LABELS: [&str; OPTIONS_ITEMS] = [
    "    Customize controls",
    "         Go to console",
    "     Reset to defaults",
    "           Screen size",
    "            Brightness",
    "           Mouse Speed",
    "       CD Music Volume",
    "          Sound Volume",
    "            Always Run",
    "          Invert Mouse",
    "            Lookspring",
    "            Lookstrafe",
    "         Video Options",
];

/// Draw the Options submenu, a faithful port of `M_Options_Draw`: the `p_option`
/// title plaque centered at the top, the 13 right-justified labels at x=16 (8 px
/// apart from y=32), a [`draw_slider`] at x=220 for the analog rows (Screen size /
/// Brightness / Mouse Speed / CD Music Volume / Sound Volume), a [`checkbox_text`]
/// at x=220 for the boolean rows (Always Run / Invert Mouse / Lookspring /
/// Lookstrafe), and the flashing cursor glyph (12/13) at x=200 on the focused row.
///
/// A missing `conchars` leaves the labels/widgets blank but still draws the title;
/// nothing here panics. `cursor_glyph` is the flashing cursor's conchars cell
/// this frame ([`menu_cursor_glyph`]: 12/13 on real time at 4 Hz).
#[allow(clippy::too_many_arguments)]
fn draw_options_screen(
    image: &mut Image,
    menu: &Menu,
    pics: &MenuPics,
    conchars: Option<&crate::wad::Qpic>,
    scale: f32,
    ox: f32,
    oy: f32,
    cursor_glyph: u8,
    palette: &[[u8; 3]; 256],
) {
    // The "OPTIONS" title plaque, centered like the other screens' titles.
    if let Some(t) = &pics.p_option {
        let tx = (MENU_VIRT_W - t.width.max(0) as f32) * 0.5;
        blit_qpic_at(image, t, tx, 4.0, scale, ox, oy, palette);
    }

    if let Some(cc) = conchars {
        // The labels.
        for (i, label) in OPTIONS_LABELS.iter().enumerate() {
            let ry = OPTIONS_ROW_Y0 + i as f32 * OPTIONS_ROW_STEP;
            draw_string_scaled(image, cc, OPTIONS_LABEL_X, ry, label, scale, ox, oy, palette);
        }

        // The analog widgets (M_DrawSlider) on the slider rows, each with its
        // cvar's [0,1] fraction.
        let slider_row = |row: usize| OPTIONS_ROW_Y0 + row as f32 * OPTIONS_ROW_STEP;
        // Screen size: r = (scr_viewsize - 30) / (120 - 30).
        let size_frac = (menu.viewsize() - VIEWSIZE_MIN) / (VIEWSIZE_MAX - VIEWSIZE_MIN);
        draw_slider(image, cc, OPTIONS_WIDGET_X, slider_row(ROW_SCREENSIZE), size_frac, scale, ox, oy, palette);
        // Brightness: r = (1 - gamma)/0.5.
        let bright_frac = (1.0 - menu.gamma()) / (GAMMA_MAX - GAMMA_MIN);
        draw_slider(image, cc, OPTIONS_WIDGET_X, slider_row(ROW_BRIGHTNESS), bright_frac, scale, ox, oy, palette);
        // Mouse Speed: r = (sensitivity - 1)/10.
        let mouse_frac = (menu.sensitivity() - SENS_MIN) / (SENS_MAX - SENS_MIN);
        draw_slider(image, cc, OPTIONS_WIDGET_X, slider_row(ROW_MOUSESPEED), mouse_frac, scale, ox, oy, palette);
        // CD Music Volume: r = bgmvolume.
        draw_slider(image, cc, OPTIONS_WIDGET_X, slider_row(ROW_CDVOLUME), menu.bgm_volume(), scale, ox, oy, palette);
        // Sound Volume: r = volume.
        draw_slider(image, cc, OPTIONS_WIDGET_X, slider_row(ROW_SNDVOLUME), menu.volume(), scale, ox, oy, palette);

        // The checkbox rows (M_DrawCheckbox -> "on"/"off").
        let checks = [
            (ROW_ALWAYSRUN, menu.always_run()),
            (ROW_INVERTMOUSE, menu.invert_mouse()),
            (ROW_LOOKSPRING, menu.lookspring()),
            (ROW_LOOKSTRAFE, menu.lookstrafe()),
        ];
        for (row, on) in checks {
            let ry = OPTIONS_ROW_Y0 + row as f32 * OPTIONS_ROW_STEP;
            draw_string_scaled(image, cc, OPTIONS_WIDGET_X, ry, checkbox_text(on), scale, ox, oy, palette);
        }

        // The flashing cursor: M_DrawCharacter(200, 32 + cursor*8, 12 + (blink)).
        let cy = OPTIONS_ROW_Y0 + menu.cursor as f32 * OPTIONS_ROW_STEP;
        draw_char_scaled(image, cc, OPTIONS_CURSOR_X, cy, cursor_glyph, scale, ox, oy, palette);
    }
}

/// Draw the Load or Save slot list, a port of `M_Load_Draw` / `M_Save_Draw`:
/// the `p_load`/`p_save` title centered at y=4 (no qplaque on these screens),
/// [`MAX_SAVEGAMES`] rows of `M_Print(16, 32 + 8*i, m_filenames[i])` — each row
/// is the host-set slot comment, or [`UNUSED_SLOT`] when empty, exactly what
/// `M_ScanSaves` leaves for a missing `sN.sav` — and the flashing cursor at
/// `M_DrawCharacter(8, 32 + cursor*8, 12 + blink)`.
#[allow(clippy::too_many_arguments)]
fn draw_load_save_screen(
    image: &mut Image,
    menu: &Menu,
    pics: &MenuPics,
    conchars: Option<&crate::wad::Qpic>,
    scale: f32,
    ox: f32,
    oy: f32,
    cursor_glyph: u8,
    palette: &[[u8; 3]; 256],
) {
    let title = if menu.screen == MenuScreen::Save {
        &pics.p_save
    } else {
        &pics.p_load
    };
    if let Some(t) = title {
        let tx = (MENU_VIRT_W - t.width.max(0) as f32) * 0.5;
        blit_qpic_at(image, t, tx, 4.0, scale, ox, oy, palette);
    }
    if let Some(cc) = conchars {
        for i in 0..MAX_SAVEGAMES {
            let ry = 32.0 + i as f32 * 8.0;
            let text = menu.save_comment(i);
            let row = if text.is_empty() { UNUSED_SLOT } else { text };
            draw_string_scaled(image, cc, 16.0, ry, row, scale, ox, oy, palette);
        }
        let cy = 32.0 + menu.cursor as f32 * 8.0;
        draw_char_scaled(image, cc, 8.0, cy, cursor_glyph, scale, ox, oy, palette);
    }
}

/// Draw the multiplayer submenu, a port of `M_MultiPlayer_Draw`: qplaque at
/// (16,4) (drawn by the caller), the `p_multi` title centered, the `mp_menu`
/// 3-item list at (72,32), the animated menudot cursor at (54, 32 + cursor*20)
/// — and, since no net driver exists (netcode is out of scope), the C's exact
/// "No Communications Available" line at y=148, plus one port-scope note line.
#[allow(clippy::too_many_arguments)]
fn draw_multiplayer_screen(
    image: &mut Image,
    menu: &Menu,
    pics: &MenuPics,
    conchars: Option<&crate::wad::Qpic>,
    scale: f32,
    ox: f32,
    oy: f32,
    frame: usize,
    palette: &[[u8; 3]; 256],
) {
    if let Some(t) = &pics.p_multi {
        let tx = (MENU_VIRT_W - t.width.max(0) as f32) * 0.5;
        blit_qpic_at(image, t, tx, 4.0, scale, ox, oy, palette);
    }
    if let Some(l) = &pics.mp_menu {
        blit_qpic_at(image, l, 72.0, 32.0, scale, ox, oy, palette);
    }
    if let Some(dot) = pics.menudot.get(frame).and_then(|d| d.as_ref()) {
        let cy = 32.0 + menu.cursor as f32 * 20.0;
        blit_qpic_at(image, dot, 54.0, cy, scale, ox, oy, palette);
    }
    if let Some(cc) = conchars {
        // M_PrintWhite ((320/2) - ((27*8)/2), 148, "No Communications Available").
        let line = "No Communications Available";
        let cx = MENU_VIRT_W * 0.5 - (line.len() as f32 * 8.0) * 0.5;
        draw_string_scaled(image, cc, cx, 148.0, line, scale, ox, oy, palette);
        // PORT NOTE (not in the C): say *why* — multiplayer is out of scope.
        let note = "(multiplayer is not part of this port)";
        let nx = (MENU_VIRT_W - note.len() as f32 * 8.0) * 0.5;
        draw_string_scaled(image, cc, nx, 156.0, note, scale, ox, oy, palette);
    }
}

/// Draw the Customize-controls screen, a port of `M_Keys_Draw`: the `ttl_cstm`
/// title centered at y=4, the instruction line at y=32 ("Press a key..." while
/// grabbing, else "Enter to change..."), one row per [`BINDNAMES`] entry from
/// y=48 (label at x=16, bound key name(s) at x=140 — "???" when unbound, "or"
/// between two), and the cursor at x=130 — `=` while grabbing, else the
/// flashing 12/13 glyph.
#[allow(clippy::too_many_arguments)]
fn draw_keys_screen(
    image: &mut Image,
    menu: &Menu,
    pics: &MenuPics,
    conchars: Option<&crate::wad::Qpic>,
    scale: f32,
    ox: f32,
    oy: f32,
    cursor_glyph: u8,
    palette: &[[u8; 3]; 256],
) {
    if let Some(t) = &pics.ttl_cstm {
        let tx = (MENU_VIRT_W - t.width.max(0) as f32) * 0.5;
        blit_qpic_at(image, t, tx, 4.0, scale, ox, oy, palette);
    }
    let Some(cc) = conchars else { return };
    if menu.bind_grabbing() {
        draw_string_scaled(
            image, cc, 12.0, 32.0, "Press a key or button for this action", scale, ox, oy,
            palette,
        );
    } else {
        draw_string_scaled(
            image, cc, 18.0, 32.0, "Enter to change, backspace to clear", scale, ox, oy, palette,
        );
    }
    for (i, (_, label)) in BINDNAMES.iter().enumerate() {
        let y = 48.0 + 8.0 * i as f32;
        draw_string_scaled(image, cc, 16.0, y, label, scale, ox, oy, palette);
        let keys = menu.find_keys_for_command(i);
        match keys[0] {
            None => draw_string_scaled(image, cc, 140.0, y, "???", scale, ox, oy, palette),
            Some(k0) => {
                let name = keynum_to_string(k0);
                draw_string_scaled(image, cc, 140.0, y, &name, scale, ox, oy, palette);
                if let Some(k1) = keys[1] {
                    // M_Print (140 + x + 8, y, "or"); M_Print (140 + x + 32, ...).
                    let x = name.len() as f32 * 8.0;
                    draw_string_scaled(image, cc, 140.0 + x + 8.0, y, "or", scale, ox, oy, palette);
                    draw_string_scaled(
                        image, cc, 140.0 + x + 32.0, y, &keynum_to_string(k1), scale, ox, oy,
                        palette,
                    );
                }
            }
        }
    }
    let cy = 48.0 + menu.cursor as f32 * 8.0;
    if menu.bind_grabbing() {
        // M_DrawCharacter (130, 48 + keys_cursor*8, '=').
        draw_char_scaled(image, cc, 130.0, cy, b'=', scale, ox, oy, palette);
    } else {
        // M_DrawCharacter (130, 48 + keys_cursor*8, 12+((int)(realtime*4)&1)).
        draw_char_scaled(image, cc, 130.0, cy, cursor_glyph, scale, ox, oy, palette);
    }
}

/// Draw the video-modes screen — this port's `VID_MenuDraw` (vid_win.c): the
/// `vidmodes` title centered at y=4, one row per [`RESOLUTION_PRESETS`] entry
/// from y=36 (the C lists `WIDTHxHEIGHT` mode descriptions and marks the
/// current mode), the flashing cursor on the highlighted row, and hint lines.
/// Single column — the C's 3-wide grid exists to fit 15+ DOS modes; 7 presets
/// fit one column.
#[allow(clippy::too_many_arguments)]
fn draw_video_screen(
    image: &mut Image,
    menu: &Menu,
    pics: &MenuPics,
    conchars: Option<&crate::wad::Qpic>,
    scale: f32,
    ox: f32,
    oy: f32,
    cursor_glyph: u8,
    palette: &[[u8; 3]; 256],
) {
    if let Some(t) = &pics.vidmodes {
        let tx = (MENU_VIRT_W - t.width.max(0) as f32) * 0.5;
        blit_qpic_at(image, t, tx, 4.0, scale, ox, oy, palette);
    }
    let Some(cc) = conchars else { return };
    let current = menu.resolution();
    for (i, &(w, h)) in RESOLUTION_PRESETS.iter().enumerate() {
        let y = 36.0 + 8.0 * i as f32;
        let mut row = format!("{w}x{h}");
        if (w, h) == current {
            row.push_str("  (current)");
        }
        draw_string_scaled(image, cc, 16.0, y, &row, scale, ox, oy, palette);
    }
    let cy = 36.0 + menu.cursor as f32 * 8.0;
    draw_char_scaled(image, cc, 8.0, cy, cursor_glyph, scale, ox, oy, palette);
    // The C's bottom hints ("Press enter to set mode" / "Esc to exit"), at this
    // single column's foot.
    let hints_y = 36.0 + RESOLUTION_PRESETS.len() as f32 * 8.0 + 16.0;
    draw_string_scaled(
        image, cc, 9.0 * 8.0, hints_y, "Press Enter to set mode", scale, ox, oy, palette,
    );
    draw_string_scaled(
        image, cc, 15.0 * 8.0, hints_y + 16.0, "Esc to exit", scale, ox, oy, palette,
    );
}

/// Draw the Help/Ordering screen (`M_Help_Draw`): blit the current page pic
/// (`gfx/help{page}.lmp`) full-screen at virtual (0,0). A missing page pic draws
/// nothing (graceful degrade); no panic.
fn draw_help_screen(
    image: &mut Image,
    menu: &Menu,
    pics: &MenuPics,
    scale: f32,
    ox: f32,
    oy: f32,
    palette: &[[u8; 3]; 256],
) {
    if let Some(p) = pics.help.get(menu.help_page()).and_then(|p| p.as_ref()) {
        blit_qpic_at(image, p, 0.0, 0.0, scale, ox, oy, palette);
    }
}

/// Draw the Quit confirmation prompt (`M_Quit_Draw`, non-Win32 path). The real
/// engine draws `M_DrawTextBox(56,76,24,4)` with one of eight random taunts; here
/// we draw a faithful-enough centered prompt — a dark box plus a plain
/// "Are you sure you want to quit? (Y/N)" using conchars — so it works without the
/// box pics or the message table. A missing `conchars` still paints the box.
fn draw_quit_screen(
    image: &mut Image,
    conchars: Option<&crate::wad::Qpic>,
    scale: f32,
    ox: f32,
    oy: f32,
    palette: &[[u8; 3]; 256],
) {
    // A dark box behind the prompt (stand-in for M_DrawTextBox). Virtual box
    // 56,76 24x4 -> roughly x[56..264], y[76..116] in 320x200 space.
    const BOX_X0: f32 = 56.0;
    const BOX_Y0: f32 = 76.0;
    const BOX_W: f32 = 208.0;
    const BOX_H: f32 = 40.0;
    let x0 = (ox + BOX_X0 * scale).floor() as i32;
    let y0 = (oy + BOX_Y0 * scale).floor() as i32;
    let x1 = (ox + (BOX_X0 + BOX_W) * scale).ceil() as i32;
    let y1 = (oy + (BOX_Y0 + BOX_H) * scale).ceil() as i32;
    for py in y0..y1 {
        for px in x0..x1 {
            image.put(px, py, [0, 0, 0]);
        }
    }

    if let Some(cc) = conchars {
        // Two centered lines, like the C's four-line quitMessage box.
        let line1 = "Are you sure you want";
        let line2 = "to quit?  (Y / N)";
        let cx1 = (MENU_VIRT_W - line1.len() as f32 * 8.0) * 0.5;
        let cx2 = (MENU_VIRT_W - line2.len() as f32 * 8.0) * 0.5;
        draw_string_scaled(image, cc, cx1, 88.0, line1, scale, ox, oy, palette);
        draw_string_scaled(image, cc, cx2, 100.0, line2, scale, ox, oy, palette);
    }
}

// ---------------------------------------------------------------------------
// Drop-down console
// ---------------------------------------------------------------------------

/// The maximum number of scrollback lines the console retains; older lines drop
/// off the top once this is exceeded (Quake's `con_text` is a fixed ring — this
/// is the same bounded-history idea with an owned [`VecDeque`]).
pub const CONSOLE_SCROLLBACK_CAP: usize = 200;

/// The maximum length of the console input line (characters). Quake's
/// `key_lines` buffer is `MAXCMDLINE = 256`; we cap a little lower and never let
/// a runaway paste/hold grow the `String` without bound.
pub const CONSOLE_INPUT_CAP: usize = 256;

/// The fraction of the framebuffer height the console panel covers when open.
/// Quake slides the console down (`scr_con_current`); a fixed top 60% is a
/// faithful-enough stand-in for the fully-dropped console and keeps the draw
/// allocation-light and deterministic (no per-frame slide state to advance).
const CONSOLE_HEIGHT_FRAC: f32 = 0.6;

/// `Con_DrawInput` (console.c) stamps `10 + ((int)(realtime*con_cursorspeed) & 1)`
/// at the edit position: conchars cell 10 is blank and 11 is the block cursor.
const CONSOLE_CURSOR_BASE: u8 = 10;
/// `con_cursorspeed` (console.c: `float con_cursorspeed = 4;`): the input cursor
/// toggles 4 times per second of real time.
const CON_CURSORSPEED: f64 = 4.0;

/// The console input cursor's conchars cell this frame (`Con_DrawInput`):
/// `10 + ((int)(realtime*con_cursorspeed) & 1)` — blank, then the block, each
/// for a quarter second of REAL time.
pub fn console_cursor_glyph(realtime: f64) -> u8 {
    CONSOLE_CURSOR_BASE + realtime_blink_bit(realtime, CON_CURSORSPEED)
}

/// The Quake drop-down console: a panel slid over the top of the screen holding
/// a capped scrollback history plus a single editable input line. Toggled with
/// the `~` / backtick key; while open it owns the keyboard and executes the
/// commands typed into it.
///
/// The struct is pure state + editing/history methods; *drawing* is
/// [`draw_console`] and *command execution* lives in the host (the wasm shell),
/// which holds the live game world the commands act on.
pub struct Console {
    /// Whether the console is dropped down (drawn + capturing the keyboard).
    pub open: bool,
    /// Scrollback history, oldest first. Capped at [`CONSOLE_SCROLLBACK_CAP`];
    /// pushing past the cap drops the oldest line.
    lines: std::collections::VecDeque<String>,
    /// The current input line (the text after the `]` prompt), without the
    /// prompt or the cursor. Capped at [`CONSOLE_INPUT_CAP`] characters.
    input: String,
}

impl Default for Console {
    fn default() -> Self {
        Console::new()
    }
}

impl Console {
    /// A fresh, closed console with empty scrollback and input.
    pub fn new() -> Console {
        Console {
            open: false,
            lines: std::collections::VecDeque::new(),
            input: String::new(),
        }
    }

    /// Toggle the console open/closed (Quake's `Con_ToggleConsole_f`). Opening
    /// does not clear the scrollback or input — the panel slides back over the
    /// history it had.
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    /// Append one printable character to the input line, ignoring control
    /// characters and the backtick/tilde (which toggle the console, never type).
    /// A no-op once the input reaches [`CONSOLE_INPUT_CAP`] characters.
    pub fn putchar(&mut self, c: char) {
        // Only printable ASCII (and any other non-control char) is accepted; the
        // backtick and tilde are the toggle key and must never enter the buffer.
        if c == '`' || c == '~' || c.is_control() {
            return;
        }
        if self.input.chars().count() >= CONSOLE_INPUT_CAP {
            return;
        }
        self.input.push(c);
    }

    /// Delete the last character of the input line (backspace). A no-op on an
    /// empty line.
    pub fn backspace(&mut self) {
        self.input.pop();
    }

    /// The current input line (without the prompt), for the host to inspect.
    pub fn input(&self) -> &str {
        &self.input
    }

    /// Take the entered command line: echo `"]" + line` into the scrollback,
    /// clear the input, and return the line for the host to execute. Returns
    /// `None` (drawing nothing into the scrollback) when the input is blank, so
    /// pressing Enter on an empty line is a harmless no-op.
    pub fn take_input(&mut self) -> Option<String> {
        let line = std::mem::take(&mut self.input);
        if line.trim().is_empty() {
            return None;
        }
        // Echo the command into the scrollback with the `]` prompt, exactly as
        // Quake's `Con_Printf` shows the line the player just submitted.
        self.println(format!("]{line}"));
        Some(line)
    }

    /// Push one line into the scrollback, dropping the oldest line once the
    /// history exceeds [`CONSOLE_SCROLLBACK_CAP`]. Embedded newlines are split so
    /// a multi-line message counts as multiple capped lines.
    pub fn println(&mut self, line: impl Into<String>) {
        let line = line.into();
        for part in line.split('\n') {
            self.lines.push_back(part.to_string());
            while self.lines.len() > CONSOLE_SCROLLBACK_CAP {
                self.lines.pop_front();
            }
        }
    }

    /// The current number of scrollback lines (for tests / host inspection).
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Iterate the scrollback lines, oldest first (for tests / host
    /// inspection — e.g. asserting the savegame commands print the C's
    /// exact messages).
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(String::as_str)
    }

    /// Clear the scrollback history (Quake's `Con_Clear_f`). Leaves the input
    /// line untouched.
    pub fn clear(&mut self) {
        self.lines.clear();
    }
}

/// Draw the drop-down console over `image`, a port of `Con_DrawConsole` /
/// `Con_DrawInput`. When the console is closed this is a no-op (draws nothing).
///
/// When open it paints, in order:
///  1. the `conback` background ([`crate::wad::Qpic`], a 320x200 console picture)
///     stretched across the **top [`CONSOLE_HEIGHT_FRAC`]** of the frame. A
///     missing `conback` falls back to a dark fill rectangle so the panel is
///     always visible.
///  2. the last few scrollback lines, drawn bottom-up just above the input line,
///     via [`draw_string`] in the conchars font.
///  3. the input line as `"]" + input` plus the flashing cursor glyph
///     ([`console_cursor_glyph`]: cells 10/11 toggling at 4 Hz on `realtime`,
///     the C's unclamped wall clock).
///
/// Text is drawn at the same conchars scale the menu uses
/// (`scale = framebuffer_height / 200`, the 320x200 virtual canvas), so the font
/// is legible at any framebuffer size. A missing `conchars` skips all text (the
/// background still draws). Every blit is bounds-clipped; nothing panics on a
/// short/empty pic or a tiny framebuffer.
pub fn draw_console(
    image: &mut Image,
    console: &Console,
    conback: Option<&crate::wad::Qpic>,
    conchars: Option<&crate::wad::Qpic>,
    palette: &[[u8; 3]; 256],
    realtime: f64,
) {
    if !console.open || image.w == 0 || image.h == 0 {
        return;
    }

    // The panel covers the top CONSOLE_HEIGHT_FRAC of the framebuffer.
    let panel_h = ((image.h as f32 * CONSOLE_HEIGHT_FRAC).round() as usize)
        .clamp(1, image.h);

    // 1. Background. The conback is a 320x200 QPIC; stretch its FULL extent into
    //    the panel rectangle (its own aspect is ignored — Quake also stretches
    //    conback to the console width). A missing/short conback => a dark fill.
    let drew_back = match conback {
        Some(pic) if pic.width > 0 && pic.height > 0 => {
            let pw = pic.width as usize;
            let ph = pic.height as usize;
            if pic.data.len() < pw.saturating_mul(ph) {
                false
            } else {
                for py in 0..panel_h {
                    // Map this panel row back to a source texel row (nearest).
                    let sy = (py * ph) / panel_h.max(1);
                    let sy = sy.min(ph - 1);
                    for px in 0..image.w {
                        let sx = (px * pw) / image.w.max(1);
                        let sx = sx.min(pw - 1);
                        let texel = match pic.data.get(sy * pw + sx) {
                            Some(&t) => t,
                            None => continue,
                        };
                        // conback is fully opaque; index 255 stays transparent
                        // to be safe (matches the other blits).
                        if texel == HUD_TRANSPARENT {
                            continue;
                        }
                        image.put(px as i32, py as i32, palette[texel as usize]);
                    }
                }
                true
            }
        }
        _ => false,
    };
    if !drew_back {
        // Dark fill fallback so the panel is always visible without a conback.
        let fill = [10u8, 10, 14];
        for py in 0..panel_h {
            for px in 0..image.w {
                image.put(px as i32, py as i32, fill);
            }
        }
    }

    // 2 + 3. Text. Without conchars there is nothing to draw the font with.
    let Some(cc) = conchars else { return };

    // Scale the conchars to the framebuffer the same way the menu does: the
    // 320x200 virtual canvas mapped by the HEIGHT, so an 8px glyph stays 8 real
    // px at 320x200 and scales up with a larger frame.
    let scale = (image.h as f32 / MENU_VIRT_H).max(1.0);
    let glyph = 8.0 * scale; // one conchars cell, in framebuffer pixels
    let line_step = glyph; // one text row, in framebuffer pixels

    // The input line sits at the BOTTOM of the panel, with a small margin so the
    // descender isn't clipped by the panel edge.
    let margin_x = (8.0 * scale).round();
    let input_y = (panel_h as f32 - line_step - 2.0 * scale).max(0.0);

    // 3. The input line: "]" + input + the flashing cursor. Drawn directly in
    //    framebuffer pixels (scale folded into the position + the glyph block).
    let prompt = format!("]{}", console.input());
    draw_string_scaled(image, cc, 0.0, 0.0, &prompt, scale, margin_x, input_y, palette);
    // Con_DrawInput: text[key_linepos] = 10 + ((int)(realtime*con_cursorspeed)&1)
    // — the cursor cell sits at the edit position (the end of the line: this
    // console has no cursor keys) and alternates blank/block at 4 Hz.
    let cursor_col = prompt.chars().count() as f32; // 8 virtual px per char
    let glyph = console_cursor_glyph(realtime);
    draw_char_scaled(image, cc, cursor_col * 8.0, 0.0, glyph, scale, margin_x, input_y, palette);

    // 2. Scrollback: the lines just above the input, drawn bottom-up. How many
    //    rows fit between the top margin and the input line.
    let top_margin = (2.0 * scale).round();
    let avail = (input_y - top_margin).max(0.0);
    let rows = (avail / line_step).floor() as usize;
    if rows == 0 {
        return;
    }
    // Take the last `rows` scrollback lines and stack them so the newest sits
    // directly above the input line.
    let total = console.lines.len();
    let start = total.saturating_sub(rows);
    for (i, line) in console.lines.iter().skip(start).enumerate() {
        // i = 0 is the OLDEST of the shown rows (highest up); the newest sits
        // just above the input line.
        let shown = total - start; // number of lines we'll actually draw
        let row_from_bottom = (shown - 1 - i) as f32; // 0 = closest to input
        let y = input_y - line_step * (row_from_bottom + 1.0);
        if y < top_margin - line_step {
            continue;
        }
        draw_string_scaled(image, cc, 0.0, 0.0, line, scale, margin_x, y, palette);
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
            pitch: 0.0,
            roll: 0.0,
            frame: 0,
            color: [255, 32, 32],
            skinnum: 0,
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
    fn mdl_frame_verts_selects_and_resets_out_of_range() {
        use crate::mdl::TriVertex;
        let mdl = two_frame_mdl();

        // Frame 0 -> first pose.
        let f0 = mdl_frame_verts(&mdl, 0, 0.0).expect("frame 0 present");
        assert_eq!(f0.len(), 3);
        assert_eq!(f0[0], TriVertex { v: [0, 0, 0], lightnormalindex: 0 });
        assert_eq!(f0[1], TriVertex { v: [32, 0, 0], lightnormalindex: 0 });

        // Frame 1 -> second, distinct pose.
        let f1 = mdl_frame_verts(&mdl, 1, 0.0).expect("frame 1 present");
        assert_eq!(f1[0], TriVertex { v: [255, 0, 255], lightnormalindex: 0 });
        assert_eq!(f1[2], TriVertex { v: [0, 255, 255], lightnormalindex: 0 });

        // FAITHFULNESS: an out-of-range frame RESETS to 0 (R_AliasSetupFrame), it
        // does NOT clamp to the last frame.
        let reset = mdl_frame_verts(&mdl, 999, 0.0).expect("reset frame present");
        assert_eq!(reset, f0, "out-of-range frame should reset to frame 0");

        // A frameless model yields None (no pose to draw).
        let mut empty = two_frame_mdl();
        empty.frames.clear();
        assert!(mdl_frame_verts(&empty, 0, 0.0).is_none());

        // A group frame resolves to its first sub-pose at time 0 and to the next
        // sub-pose as time advances past the first interval.
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
            // time in [0,0.1) -> sub-pose 0; time in [0.1,0.2) -> sub-pose 1.
            let g0 = mdl_frame_verts(&grouped, 0, 0.05).expect("group sub-pose 0");
            assert_eq!(g0[0], TriVertex { v: [7, 0, 0], lightnormalindex: 0 });
            let g1 = mdl_frame_verts(&grouped, 0, 0.15).expect("group sub-pose 1");
            assert_eq!(g1[0], TriVertex { v: [50, 0, 0], lightnormalindex: 0 });
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
        let sk = mdl_skin(&mdl, 0, 0.0).expect("1x1 single skin resolves");
        assert_eq!((sk.width, sk.height), (1, 1));
        assert_eq!(sk.pixels.len(), 1);

        // A 2x2 single skin with the right number of pixels resolves.
        mdl.header.skinwidth = 2;
        mdl.header.skinheight = 2;
        mdl.skins = vec![Skin::Single(vec![1, 2, 3, 4])];
        let sk = mdl_skin(&mdl, 0, 0.0).expect("2x2 single skin resolves");
        assert_eq!((sk.width, sk.height), (2, 2));
        assert_eq!(sk.pixels, &[1, 2, 3, 4]);
        // An out-of-range skinnum resets to 0 (it does not reject the model).
        let sk_oob = mdl_skin(&mdl, 99, 0.0).expect("out-of-range skinnum resets to 0");
        assert_eq!(sk_oob.pixels, &[1, 2, 3, 4]);

        // Too few pixels for the claimed dimensions -> None (flat fallback).
        mdl.skins = vec![Skin::Single(vec![1, 2, 3])];
        assert!(mdl_skin(&mdl, 0, 0.0).is_none(), "short pixel buffer must be rejected");

        // Non-positive dimensions -> None.
        let mut zero = tiny_mdl();
        zero.header.skinwidth = 0;
        assert!(mdl_skin(&zero, 0, 0.0).is_none(), "zero skinwidth must be rejected");

        // No skins at all -> None.
        let mut noskin = tiny_mdl();
        noskin.skins.clear();
        assert!(mdl_skin(&noskin, 0, 0.0).is_none(), "skinless model must be rejected");

        // A skin GROUP animates by time: sub-skin 0 for t<0.1, sub-skin 1 after.
        let mut grouped = tiny_mdl();
        grouped.header.skinwidth = 2;
        grouped.header.skinheight = 1;
        grouped.skins = vec![Skin::Group {
            intervals: vec![0.1, 0.2],
            frames: vec![vec![5, 6], vec![7, 8]],
        }];
        let sk = mdl_skin(&grouped, 0, 0.05).expect("skin group resolves at t=0.05");
        assert_eq!((sk.width, sk.height), (2, 1));
        assert_eq!(sk.pixels, &[5, 6], "group sub-skin 0 at t=0.05");
        let sk2 = mdl_skin(&grouped, 0, 0.15).expect("skin group resolves at t=0.15");
        assert_eq!(sk2.pixels, &[7, 8], "group sub-skin 1 at t=0.15 (animates)");
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
            pitch: 0.0,
            roll: 0.0,
            frame: 0,
            color: [255, 32, 32],
            skinnum: 0,
        };
        let img_skin = render_scene(&bsp, &cam, 160, 120, &pal, std::slice::from_ref(&inst_skin));

        // Same model/instance but with the skin stripped -> flat fallback path.
        let mut flat = skinned_mdl();
        flat.skins.clear();
        let inst_flat = ModelInstance {
            mdl: &flat,
            origin: [-80.0, 0.0, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            frame: 0,
            color: [255, 32, 32],
            skinnum: 0,
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
            pitch: 0.0,
            roll: 0.0,
            frame: 0,
            color: [255, 32, 32],
            skinnum: 0,
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
            pitch: 0.0,
            roll: 0.0,
            frame: 0,
            color: [255, 32, 32],
            skinnum: 0,
        };
        let inst1 = ModelInstance {
            mdl: &mdl,
            origin: [-80.0, 0.0, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            frame: 1,
            color: [255, 32, 32],
            skinnum: 0,
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
        let lm = face_lightmap_dyn(&bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&dl), ALL_DLIGHT_BITS)
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
    fn unlit_face_hit_by_dlight_is_not_fullbright() {
        // FIX 6: a NORMAL wall with no baked lightmap (lightofs < 0) is fullbright
        // with no dlights, but a reaching dynamic light must build a lightmap (zero
        // base + the light) instead of staying fullbright.
        let (bsp, face, poly) = one_face_bsp_zplane(0);
        // Force lightofs < 0 (no baked samples) but keep the face NORMAL (flags 0).
        let mut unlit = face.clone();
        unlit.lightofs = -1;

        // With no dlights: fullbright (None).
        assert!(
            face_lightmap_dyn(&bsp, &unlit, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, &[], 0).is_none(),
            "an unlit face with no dlights stays fullbright"
        );

        // A bright light 16 units above luxel (0,0): the face is now lightmapped,
        // owning a buffer, bright near the impact and dark (not fullbright) away.
        let dl = DynamicLight::new([0.0, 0.0, 16.0], 60.0, 10.0, 0.0, 0.0, 0);
        let lm = face_lightmap_dyn(&bsp, &unlit, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&dl), ALL_DLIGHT_BITS)
            .expect("a reaching dlight must build a lightmap for the unlit face");
        assert!(matches!(lm.luxels, Luxels::Owned(_)), "reaching dlight owns the buffer");
        // Near luxel brightens above the zero base; far luxel stays at ~0 (dark,
        // NOT fullbright — which is the whole point of the fix).
        let near = lm.factor_at(0.0, 0.0);
        let far = lm.factor_at(32.0, 32.0);
        assert!(near > 0.1, "near the dlight the unlit face lights up: {near}");
        assert!(far < 0.05, "away from the dlight the unlit face is dark, not fullbright: {far}");

        // A far-away dlight that never reaches leaves the face fullbright (None),
        // so the common case (dlights elsewhere in the level) is unchanged.
        let far_dl = DynamicLight::new([0.0, 0.0, 100_000.0], 200.0, 10.0, 0.0, 0.0, 0);
        assert!(
            face_lightmap_dyn(&bsp, &unlit, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&far_dl), ALL_DLIGHT_BITS).is_none(),
            "a non-reaching dlight leaves the unlit face fullbright"
        );
    }

    /// `one_face_bsp_zplane` with the face's surfedge/edge/vertex walk wired to
    /// the SAME 0..32 square as the hand-made poly, so [`any_dlight_reaches`]'s
    /// internal extent walk sees the real face bounds.
    fn one_face_bsp_zplane_with_edges(luxel: u8) -> (Bsp, crate::bsp::DFace, Vec<Vec3>) {
        let (mut bsp, face, poly) = one_face_bsp_zplane(luxel);
        bsp.vertexes = poly
            .iter()
            .map(|&point| crate::bsp::DVertex { point })
            .collect();
        bsp.edges = (0..4)
            .map(|i| crate::bsp::DEdge {
                v: [i as u16, ((i + 1) % 4) as u16],
            })
            .collect();
        bsp.surfedges = vec![0, 1, 2, 3];
        (bsp, face, poly)
    }

    #[test]
    fn dlight_reach_is_spatial_not_just_planar() {
        // Regression for the live-play "pop": `any_dlight_reaches` used ONLY the
        // plane-distance test, so a dlight anywhere near-coplanar with a face —
        // even 2000+ units away laterally (the start map's lava fireballs vs the
        // spawn hall's floors) — kicked the face off the baked surface cache
        // onto the per-pixel path and the WHOLE view's shading visibly shifted
        // whenever any dlight existed. WinQuake's R_MarkLights recurses the BSP
        // from the light, bounded to ±radius at every split, so distant faces
        // are never marked. The gate now also tests the face's texture-space
        // extent with R_AddDynamicLights' own distance estimate.
        let (bsp, face, _poly) = one_face_bsp_zplane_with_edges(100);

        // Near light above the face: reaches (and genuinely lights luxels).
        let near = DynamicLight::new([0.0, 0.0, 16.0], 60.0, 10.0, 0.0, 0.0, 0);
        assert!(
            any_dlight_reaches(&bsp, &face, std::slice::from_ref(&near), ALL_DLIGHT_BITS),
            "a light directly above the face reaches it"
        );

        // Coplanar-but-distant light (the fireball case): 16 above the z=0
        // plane like `near`, but 5000 units away laterally. The old plane-only
        // gate said REACHES (rad = 200-16 = 184 >= 0); it must not.
        let coplanar_far = DynamicLight::new([5000.0, 0.0, 16.0], 200.0, 10.0, 0.0, 0.0, 0);
        assert!(
            !any_dlight_reaches(&bsp, &face, std::slice::from_ref(&coplanar_far), ALL_DLIGHT_BITS),
            "a laterally distant coplanar light must NOT mark the face"
        );
        // ...and add_dynamic_lights agrees it contributes nothing: every luxel
        // keeps its static value (the gate is exactly "would at least one luxel
        // receive light"; the owned-vs-borrowed buffer kind is an implementation
        // detail of the lazy materialisation).
        let lm = face_lightmap_dyn(
            &bsp,
            &face,
            &_poly,
            &NEUTRAL_LIGHTSTYLE_SCALES,
            std::slice::from_ref(&coplanar_far),
            ALL_DLIGHT_BITS,
        )
        .expect("lightmap present");
        let static_factor = 100.0 / 255.0 * 2.0;
        for &(s, t) in &[(0.0, 0.0), (16.0, 16.0), (32.0, 32.0)] {
            assert!(
                (lm.factor_at(s, t) - static_factor).abs() < 1e-6,
                "the distant light adds nothing at ({s},{t})"
            );
        }

        // Far along the normal: fails the plane test as before.
        let above = DynamicLight::new([0.0, 0.0, 300.0], 200.0, 10.0, 0.0, 0.0, 0);
        assert!(
            !any_dlight_reaches(&bsp, &face, std::slice::from_ref(&above), ALL_DLIGHT_BITS),
            "a light beyond its radius along the normal does not reach"
        );

        // Rim case just inside: impact at s=32+40=72 -> sd=40 (extent quantizes
        // to 0..32), td=0, dist2=40 < reach (60-16=44): reaches.
        let rim = DynamicLight::new([72.0, 16.0, 16.0], 60.0, 10.0, 0.0, 0.0, 0);
        assert!(
            any_dlight_reaches(&bsp, &face, std::slice::from_ref(&rim), ALL_DLIGHT_BITS),
            "a light just within the dist2 estimate of the extent reaches"
        );
        // ...and just outside: sd=48 > 44: does not reach.
        let rim_out = DynamicLight::new([80.0, 16.0, 16.0], 60.0, 10.0, 0.0, 0.0, 0);
        assert!(
            !any_dlight_reaches(&bsp, &face, std::slice::from_ref(&rim_out), ALL_DLIGHT_BITS),
            "a light just outside the dist2 estimate does not reach"
        );

        // Empty slice: never reaches (the no-dlight fast path).
        assert!(!any_dlight_reaches(&bsp, &face, &[], ALL_DLIGHT_BITS));
    }

    #[test]
    fn texture_animation_selects_frame_by_time() {
        // FIX 1: a parsed BSP with a 2-frame `+`-animated cycle selects the frame
        // by (int)(time*10) % anim_total via R_TextureAnimation.
        use crate::bsp::{MipTex, TexAnim};
        let mut bsp = demo_room();
        let mk = |name: &str, anim: Option<TexAnim>| MipTex {
            name: name.into(),
            width: 16,
            height: 16,
            offsets: [0, 0, 0, 0],
            pixels: vec![0u8; 16 * 16],
            anim,
        };
        // Two-frame primary cycle: frame 0 (idx 0) and frame 1 (idx 1), ANIM_CYCLE=2.
        // anim_total = 2*2 = 4 tenths. Frame 0 covers rel in [0,2), frame 1 [2,4).
        bsp.textures = vec![
            Some(mk("+0wat", Some(TexAnim { total: 4, min: 0, max: 2, next: 1, alternate: None }))),
            Some(mk("+1wat", Some(TexAnim { total: 4, min: 2, max: 4, next: 0, alternate: None }))),
        ];

        // time=0.0 -> rel=0 -> frame 0 (index 0).
        assert_eq!(texture_animation(&bsp, 0, 0, 0.0), 0);
        // time=0.1 -> rel=1 -> still frame 0.
        assert_eq!(texture_animation(&bsp, 0, 0, 0.1), 0);
        // time=0.2 -> rel=2 -> frame 1 (index 1).
        assert_eq!(texture_animation(&bsp, 0, 0, 0.2), 1);
        // time=0.4 -> rel=0 (wraps) -> frame 0.
        assert_eq!(texture_animation(&bsp, 0, 0, 0.4), 0);
        // Starting the walk from frame 1's index resolves the same frame for a
        // given time (R_TextureAnimation walks anim_next from any base).
        assert_eq!(texture_animation(&bsp, 1, 0, 0.0), 0);
        assert_eq!(texture_animation(&bsp, 1, 0, 0.2), 1);

        // A non-animated texture returns its own index unchanged.
        bsp.textures.push(Some(mk("brick", None)));
        assert_eq!(texture_animation(&bsp, 2, 0, 5.0), 2);
        // An out-of-range base index returns it unchanged (no panic).
        assert_eq!(texture_animation(&bsp, 99, 0, 1.0), 99);
    }

    #[test]
    fn texture_animation_entity_frame_selects_alternate() {
        // FIX 1: with the drawing entity's frame != 0, switch to the alternate
        // cycle (R_TextureAnimation `if (currententity->frame) base = alternate`).
        use crate::bsp::{MipTex, TexAnim};
        let mut bsp = demo_room();
        let mk = |name: &str, anim: TexAnim| MipTex {
            name: name.into(),
            width: 16,
            height: 16,
            offsets: [0, 0, 0, 0],
            pixels: vec![0u8; 16 * 16],
            anim: Some(anim),
        };
        // Primary frame at index 0 with alternate -> index 1 (a single-frame alt).
        bsp.textures = vec![
            Some(mk("+0sw", TexAnim { total: 2, min: 0, max: 2, next: 0, alternate: Some(1) })),
            Some(mk("+asw", TexAnim { total: 2, min: 0, max: 2, next: 1, alternate: Some(0) })),
        ];
        // ent_frame 0 -> primary (index 0).
        assert_eq!(texture_animation(&bsp, 0, 0, 0.0), 0);
        // ent_frame != 0 -> alternate cycle's first frame (index 1).
        assert_eq!(texture_animation(&bsp, 0, 1, 0.0), 1);
    }

    #[test]
    fn empty_dlights_keeps_static_borrow_and_factor() {
        // With no dlights the lightmap must borrow the static bytes and sample
        // exactly the pre-dlight factor (byte-identical behaviour).
        let (bsp, face, poly) = one_face_bsp_zplane(150);
        let with_none = face_lightmap_dyn(&bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, &[], 0).expect("present");
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
        let lm = face_lightmap_dyn(&bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&dl), ALL_DLIGHT_BITS).expect("present");
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
        let lm = face_lightmap_dyn(&bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&dl), ALL_DLIGHT_BITS).expect("present");
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
        let lm = face_lightmap_dyn(&bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, &[], 0)
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
        let dark = face_lightmap_dyn(&bsp, &face, &poly, &scales, &[], 0).expect("present");
        assert!(matches!(dark.luxels, Luxels::Owned(_)), "2-style face owns the combine");
        // Effective luxel 100 -> factor 100/255*2.
        assert!((dark.factor_at(0.0, 0.0) - (100.0 / 255.0 * 2.0)).abs() < 1e-5);

        // Style 1 normal (scale 1): effective = 100 + 200 = 300 -> clamps in factor.
        let mut scales_on = NEUTRAL_LIGHTSTYLE_SCALES;
        scales_on[1] = 1.0;
        let bright = face_lightmap_dyn(&bsp, &face, &poly, &scales_on, &[], 0).expect("present");
        let f_dark = dark.factor_at(0.0, 0.0);
        let f_bright = bright.factor_at(0.0, 0.0);
        assert!(
            f_bright > f_dark + 0.5,
            "raising style-1 scale must brighten the effective luxel: {f_dark} -> {f_bright}"
        );

        // And a partial scale lands strictly between (proves it scales the block).
        let mut scales_half = NEUTRAL_LIGHTSTYLE_SCALES;
        scales_half[1] = 0.5; // effective = 100 + 100 = 200
        let mid = face_lightmap_dyn(&bsp, &face, &poly, &scales_half, &[], 0).expect("present");
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
        let lm = face_lightmap_dyn(&bsp, &face, &poly, &scales, &[], 0).expect("present");
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
        let lm = face_lightmap_dyn(&bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, &[], 0)
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

        let base = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None);
        // demo_room has no lighting lump, so faces are fullbright (no lightmap)
        // and dlights cannot attach; the frame must therefore be UNCHANGED even
        // with a light present -- proving dlights never touch non-lightmapped
        // faces and never panic.
        let dl = DynamicLight::new([0.0, 0.0, 0.0], 600.0, 10.0, 0.0, 0.0, 0);
        let lit = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], std::slice::from_ref(&dl), &NEUTRAL_LIGHTSTYLE_SCALES, None);
        assert_eq!(base.rgb, lit.rgb, "fullbright (lightmap-less) world must ignore dlights");
    }

    // -- R_MarkLights BSP dlight gating (r_light.c) -------------------------

    /// Two rooms separated by a solid wall split at `x=0`, floored at `z=0` by
    /// two COPLANAR faces — face 0 (near room, `x>0`) and face 1 (far room,
    /// `x<0`) — with a real `R_MarkLights` node tree:
    ///
    /// ```text
    ///   node 0 (root): plane 0 (wall x=0), no faces, children [node 1, node 2]
    ///   node 1 (x>0):  plane 1 (floor z=0), owns face 0, children = leaves
    ///   node 2 (x<0):  plane 1 (floor z=0), owns face 1, children = leaves
    /// ```
    ///
    /// Both floors are lightmapped (uniform luxels = 100) and the shared texinfo
    /// S/T axes are scaled by 1/4 (a 4x texture scale, legal in Quake), so
    /// lightmap-space distances are a quarter of world distances. That is
    /// exactly the configuration where the old proximity-only dlight gating
    /// visibly lit the far room's wall-adjacent floor luxels through the wall
    /// (the two floors share one plane, and the lit luxels sit within the
    /// light's lightmap-space reach) while the C's BSP recursion never descends
    /// past a split the light's sphere does not touch.
    fn two_rooms_bsp() -> Bsp {
        use crate::bsp::{DEdge, DFace, DModel, DNode, DPlane, DVertex, TexInfo};

        let mut vertexes: Vec<DVertex> = Vec::new();
        let mut edges: Vec<DEdge> = vec![DEdge { v: [0, 0] }]; // edge 0 unused
        let mut surfedges: Vec<i32> = Vec::new();
        let mut faces: Vec<DFace> = Vec::new();

        // plane 0: the solid wall split at x=0; plane 1: the shared floor z=0.
        let planes = vec![
            DPlane { normal: [1.0, 0.0, 0.0], dist: 0.0, ptype: 0 },
            DPlane { normal: [0.0, 0.0, 1.0], dist: 0.0, ptype: 2 },
        ];

        // One floor quad per room (z=0, +Z normal, CCW seen from above so a
        // camera above passes the backface cull).
        let mut add_floor = |x0: f32, x1: f32| {
            let base = vertexes.len() as u16;
            for c in [[x0, -128.0, 0.0], [x1, -128.0, 0.0], [x1, 128.0, 0.0], [x0, 128.0, 0.0]] {
                vertexes.push(DVertex { point: c });
            }
            let first_edge = surfedges.len() as i32;
            for k in 0..4u16 {
                let e = edges.len() as i32;
                edges.push(DEdge { v: [base + k, base + (k + 1) % 4] });
                surfedges.push(e);
            }
            faces.push(DFace {
                planenum: 1,
                side: 0,
                firstedge: first_edge,
                numedges: 4,
                texinfo: 0,
                styles: [0, 255, 255, 255],
                lightofs: 0,
            });
        };
        add_floor(0.0, 256.0); // face 0: near room
        add_floor(-256.0, 0.0); // face 1: far room

        // The marking tree (children < 0 are leaf references and end a descent).
        let nodes = vec![
            DNode { planenum: 0, children: [1, 2], mins: [-256, -128, -16], maxs: [256, 128, 16], firstface: 0, numfaces: 0 },
            DNode { planenum: 1, children: [-1, -2], mins: [0, -128, -16], maxs: [256, 128, 16], firstface: 0, numfaces: 1 },
            DNode { planenum: 1, children: [-3, -4], mins: [-256, -128, -16], maxs: [0, 128, 16], firstface: 1, numfaces: 1 },
        ];

        // 1/4-scale S/T axes (4x texture scale): face extents s,t in [-64..64],
        // so each floor is a 5x5 luxel grid (25 bytes per face; lightofs 0).
        let texinfo = vec![TexInfo {
            vecs: [[0.25, 0.0, 0.0, 0.0], [0.0, 0.25, 0.0, 0.0]],
            miptex: 0,
            flags: 0,
        }];

        let models = vec![DModel {
            mins: [-256.0, -128.0, -16.0],
            maxs: [256.0, 128.0, 16.0],
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
            nodes,
            leafs: Vec::new(),
            clipnodes: Vec::new(),
            texinfo,
            models,
            marksurfaces: Vec::new(),
            surfedges,
            textures: Vec::new(),
            visibility: Vec::new(),
            lighting: vec![100u8; 64],
        }
    }

    #[test]
    fn mark_lights_prunes_subtree_beyond_solid_split() {
        let bsp = two_rooms_bsp();
        // Light in the NEAR room, 40 units from the wall, radius 36: the sphere
        // never crosses the x=0 split, so the far room's subtree is never
        // descended (C `if (dist > light->radius)` recurses children[0] only).
        let dl = DynamicLight::new([40.0, 0.0, 8.0], 36.0, 10.0, 0.0, 0.0, 0);
        let mut bits = Vec::new();
        mark_dlights(&bsp, 0, std::slice::from_ref(&dl), &mut bits);
        assert_eq!(bits.len(), 2);
        assert_eq!(bits[0], 1, "near-room floor must carry light 0's bit");
        assert_eq!(bits[1], 0, "far-room floor must NOT be marked across the solid split");

        // The pure proximity test (the pre-R_MarkLights gating) WOULD have
        // flagged the far face as touched — the floors are coplanar, so the
        // plane distance (8) is well inside the radius (36). This is exactly
        // the divergence the BSP recursion fixes.
        assert!(
            any_dlight_reaches(&bsp, &bsp.faces[1], std::slice::from_ref(&dl), ALL_DLIGHT_BITS),
            "sanity: by plane distance alone the far floor is in reach"
        );
        assert!(
            !any_dlight_reaches(&bsp, &bsp.faces[1], std::slice::from_ref(&dl), bits[1]),
            "with the real mask the far floor reports untouched"
        );
    }

    #[test]
    fn mark_lights_straddling_split_marks_both_sides() {
        let bsp = two_rooms_bsp();
        // The same light moved to 10 units from the wall: its sphere straddles
        // the split, so BOTH children are descended and both floors are marked
        // (the C marks the node's own faces and recurses both sides).
        let dl = DynamicLight::new([10.0, 0.0, 8.0], 36.0, 10.0, 0.0, 0.0, 0);
        let mut bits = Vec::new();
        mark_dlights(&bsp, 0, std::slice::from_ref(&dl), &mut bits);
        assert_eq!(bits[0], 1, "near-room floor marked");
        assert_eq!(bits[1], 1, "far-room floor marked: the sphere reaches across the split");
    }

    #[test]
    fn mark_lights_per_light_bits_accumulate() {
        let bsp = two_rooms_bsp();
        // Light 0 stays in the near room; light 1 straddles the split. Face 0
        // accumulates both bits, face 1 only light 1's (1 << 1) — per-light
        // masks exactly like the C `surf->dlightbits |= bit`.
        let dls = [
            DynamicLight::new([40.0, 0.0, 8.0], 36.0, 10.0, 0.0, 0.0, 0),
            DynamicLight::new([10.0, 0.0, 8.0], 36.0, 10.0, 0.0, 0.0, 0),
        ];
        let mut bits = Vec::new();
        mark_dlights(&bsp, 0, &dls, &mut bits);
        assert_eq!(bits[0], 0b11);
        assert_eq!(bits[1], 0b10);
    }

    #[test]
    fn mark_lights_no_node_tree_falls_back_to_all_marked() {
        // A synthetic map with no node tree (demo_room) cannot recurse: every
        // face falls back to "marked by every light" so the per-light distance
        // test in add_dynamic_lights remains the only gate (pre-gating
        // behaviour). With no live lights the scratch stays empty (mask 0).
        let bsp = demo_room();
        assert!(bsp.nodes.is_empty(), "fixture: demo_room must have no nodes");
        let dl = DynamicLight::new([0.0, 0.0, 0.0], 200.0, 10.0, 0.0, 0.0, 0);
        let mut bits = Vec::new();
        mark_dlights(&bsp, 0, std::slice::from_ref(&dl), &mut bits);
        assert_eq!(bits.len(), bsp.faces.len());
        assert!(bits.iter().all(|&b| b == ALL_DLIGHT_BITS));
        mark_dlights(&bsp, 0, &[], &mut bits);
        assert!(bits.is_empty(), "no live lights -> empty scratch (reads as mask 0)");
    }

    #[test]
    fn mark_lights_terminates_on_malformed_cyclic_tree() {
        // A node whose children point back at itself must terminate via the
        // visit budget (never hang or overflow the stack) — real trees visit
        // each node at most once per light.
        let mut bsp = two_rooms_bsp();
        bsp.nodes[0].children = [0, 0];
        let dl = DynamicLight::new([10.0, 0.0, 8.0], 36.0, 10.0, 0.0, 0.0, 0);
        let mut bits = Vec::new();
        mark_dlights(&bsp, 0, std::slice::from_ref(&dl), &mut bits); // must return
        // Out-of-range headnode is harmless too.
        mark_dlights(&bsp, 999, std::slice::from_ref(&dl), &mut bits);
        assert!(bits.iter().all(|&b| b == 0));
    }

    #[test]
    fn unmarked_face_ignores_geometrically_reaching_dlight() {
        // A light that reaches the face by pure distance must still be ignored
        // when its R_MarkLights bit is clear — the C only folds lights present
        // in `surf->dlightbits` (`R_AddDynamicLights`).
        let (bsp, face, poly) = one_face_bsp_zplane(100);
        let dl = DynamicLight::new([0.0, 0.0, 16.0], 60.0, 10.0, 0.0, 0.0, 0);

        // Mask 0: the buffer stays the static borrow, as if the light were absent.
        let lm = face_lightmap_dyn(
            &bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&dl), 0,
        )
        .expect("lightmap present");
        assert!(
            matches!(lm.luxels, Luxels::Static(_)),
            "an unmarked light must not touch the lightmap"
        );

        // Bit 0 set: the very same light brightens (owned buffer).
        let lit = face_lightmap_dyn(
            &bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&dl), 1,
        )
        .expect("lightmap present");
        assert!(matches!(lit.luxels, Luxels::Owned(_)), "a marked reaching light applies");
    }

    /// Project a world point through the same camera basis / focal math the
    /// world pass uses, returning the (clamped) target pixel.
    fn project_px(cam: &Camera, w: usize, h: usize, p: Vec3) -> (usize, usize) {
        let (forward, right, up) = cam.basis();
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        let tan_half = (cam.fov_deg as f64 * 0.5).to_radians().tan();
        let focal = (cx as f64 / tan_half) as f32;
        let rel = sub(p, cam.pos);
        let vz = dot(rel, forward).max(1e-3);
        let x = cx + focal * dot(rel, right) / vz;
        let y = cy - focal * dot(rel, up) / vz;
        (
            (x as usize).min(w.saturating_sub(1)),
            (y as usize).min(h.saturating_sub(1)),
        )
    }

    #[test]
    fn dlight_does_not_bleed_into_bsp_region_it_cannot_reach() {
        // End-to-end: render the two-room map with a dlight whose sphere stays
        // inside the near room. The near floor must brighten; the far floor —
        // coplanar, with wall-adjacent luxels inside the light's lightmap-space
        // reach, i.e. visibly lit by the OLD proximity-only gating — must be
        // byte-identical to the unlit frame.
        reset_render_caches();
        let bsp = two_rooms_bsp();
        let pal = [[128u8, 128, 128]; 256];
        // Above and behind the origin, looking down across both rooms.
        let cam = Camera::looking_at([0.0, -220.0, 260.0], [0.0, 0.0, 0.0], 90.0);
        let (w, h) = (200usize, 150usize);

        let base = render_scene_ext(
            &bsp, &cam, w, h, &pal, &[], &[], &[], None, 0.0, &[], &[],
            &NEUTRAL_LIGHTSTYLE_SCALES, None,
        );
        let dl = DynamicLight::new([40.0, 0.0, 8.0], 36.0, 10.0, 0.0, 0.0, 0);
        let lit = render_scene_ext(
            &bsp, &cam, w, h, &pal, &[], &[], &[], None, 0.0, &[],
            std::slice::from_ref(&dl), &NEUTRAL_LIGHTSTYLE_SCALES, None,
        );

        let px = |img: &Image, p: Vec3| -> [u8; 3] {
            let (x, y) = project_px(&cam, w, h, p);
            img.rgb[y * w + x]
        };

        // Near floor under the light brightens.
        assert_ne!(
            px(&base, [40.0, 0.0, 0.0]),
            px(&lit, [40.0, 0.0, 0.0]),
            "near-room floor must brighten under the dlight"
        );
        // Far floor: every sample byte-identical (no light through the wall).
        for sample in [
            [-8.0, 0.0, 0.0],
            [-40.0, 0.0, 0.0],
            [-72.0, 0.0, 0.0],
            [-40.0, 64.0, 0.0],
            [-40.0, -64.0, 0.0],
        ] {
            assert_eq!(
                px(&base, sample),
                px(&lit, sample),
                "far-room floor lit through the wall at {sample:?}"
            );
        }
    }

    // -- PVS culling: decompress_vis / point_in_leaf -----------------------

    #[test]
    // The literal leaf-index ranges ARE the assertion; iterators would obscure them.
    #[allow(clippy::needless_range_loop)]
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
        // Exactly on the plane (d == 0) goes to the BACK child (strict `d > 0`,
        // matching C Mod_PointInLeaf) -> leaf 2.
        assert_eq!(point_in_leaf(&bsp, [0.0, 5.0, -3.0]), Some(2));
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

        let without = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None);
        let with = render_scene_ext(
            &bsp,
            &cam,
            160,
            120,
            &pal,
            &[],
            // Place the quad between the camera (-200) and the centre, facing it.
            &[BModelInstance { model_index: 1, origin: [-120.0, 0.0, 0.0], frame: 0 }],
            &[],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );

        let drawn_without = without.rgb.iter().filter(|&&p| p != bg).count();
        let drawn_with = with.rgb.iter().filter(|&&p| p != bg).count();
        // The submodel quad sits at world x = -120, nearer than the world walls
        // behind it, so it must paint at least as many non-background pixels as
        // without it (it can only add coverage, never remove it). With near-plane
        // clipping the surrounding world walls now also fill the frame, so this is
        // an `>=` rather than a strict `>` — the strong check below is occlusion.
        assert!(
            drawn_with >= drawn_without,
            "submodel must not reduce coverage: {drawn_without} -> {drawn_with}"
        );

        // The decisive check: the submodel is nearer than the geometry behind it,
        // so adding it must CHANGE the framebuffer (it occludes the far wall). This
        // proves the submodel is rasterised and depth-tested, independent of how
        // much background the world fills.
        let changed = without
            .rgb
            .iter()
            .zip(with.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "submodel changed no pixels (not drawn / fully occluded)");
    }

    #[test]
    fn render_scene_ext_out_of_range_submodel_is_noop() {
        // An out-of-range model_index must draw nothing and not panic: the image
        // is byte-identical to passing an empty bmodel list.
        let bsp = demo_room_with_submodel();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);

        let empty = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None);
        let oob = render_scene_ext(
            &bsp,
            &cam,
            160,
            120,
            &pal,
            &[],
            &[BModelInstance { model_index: 999, origin: [-120.0, 0.0, 0.0], frame: 0 }],
            &[],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
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
        let b = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None);
        assert_eq!(a.rgb, b.rgb, "render_scene must equal render_scene_ext(.., &[])");

        // Also holds with an alias instance present (the model path is shared).
        let mdl = tiny_mdl();
        let inst = ModelInstance {
            mdl: &mdl,
            origin: [-80.0, 0.0, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            frame: 0,
            color: [255, 32, 32],
            skinnum: 0,
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
            &[],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
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
            &[BModelInstance { model_index: 1, origin: [-120.0, 0.0, 0.0], frame: 0 }],
            &[],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
        // Shift the quad well off to one side (+Y) so it projects elsewhere.
        let shifted = render_scene_ext(
            &bsp,
            &cam,
            160,
            120,
            &pal,
            &[],
            &[BModelInstance { model_index: 1, origin: [-120.0, 120.0, 0.0], frame: 0 }],
            &[],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
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
            &[BModelInstance { model_index: 1, origin: [-120.0, 0.0, 0.0], frame: 0 }],
            &[],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
    }

    // -- External brush models (standalone b_*.bsp item boxes) -----------------

    /// A standalone tiny brush BSP (version 29) whose **MODEL 0** is a single
    /// 64x64 YZ quad at local x = 0, facing -X (outward normal -X) — the smallest
    /// stand-in for Quake's `maps/b_*.bsp` item boxes. Built around the bsp's own
    /// local origin so [`ExternalBModel`] / [`draw_brush_bsp`] place it at a world
    /// origin via the same origin shift the submodel path uses. No inline textures
    /// (so it takes the flat-colour fallback) and no lighting lump (fullbright).
    fn tiny_brush_bsp() -> Bsp {
        use crate::bsp::{DEdge, DFace, DModel, DPlane, DVertex, TexInfo, PLANE_X};

        // Four corners of a 64x64 YZ quad at local x = 0, ordered CCW as seen from
        // -X (so with the -X plane normal the face is visible from a -X camera).
        let vertexes = vec![
            DVertex { point: [0.0, -32.0, -32.0] },
            DVertex { point: [0.0, 32.0, -32.0] },
            DVertex { point: [0.0, 32.0, 32.0] },
            DVertex { point: [0.0, -32.0, 32.0] },
        ];
        // Edge 0 is conventionally unused; reserve a dummy then four real edges.
        let mut edges = vec![DEdge { v: [0, 0] }];
        let mut surfedges: Vec<i32> = Vec::new();
        for k in 0..4u16 {
            let a = k;
            let b = (k + 1) % 4;
            let edge_index = edges.len() as i32;
            edges.push(DEdge { v: [a, b] });
            surfedges.push(edge_index);
        }
        let planes = vec![DPlane { normal: [-1.0, 0.0, 0.0], dist: 0.0, ptype: PLANE_X }];
        let texinfo = vec![TexInfo {
            vecs: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
            miptex: 0,
            flags: 0,
        }];
        let faces = vec![DFace {
            planenum: 0,
            side: 0,
            firstedge: 0,
            numedges: 4,
            texinfo: 0,
            styles: [0, 0, 0, 0],
            lightofs: -1,
        }];
        // MODEL 0 covers the single face — the whole little box brush.
        let models = vec![DModel {
            mins: [-1.0, -32.0, -32.0],
            maxs: [1.0, 32.0, 32.0],
            origin: [0.0, 0.0, 0.0],
            headnode: [0, 0, 0, 0],
            visleafs: 0,
            firstface: 0,
            numfaces: 1,
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

    #[test]
    fn render_scene_ext_draws_external_brush_bsp() {
        // An external brush model placed in front of the camera must add /change
        // pixels relative to an empty `external` list — proving its MODEL-0 faces
        // are rasterised at the entity origin and depth-tested against the world.
        let world = demo_room();
        let box_bsp = tiny_brush_bsp();
        let pal = [[200u8, 200, 200]; 256];
        // Look down +X from near the west wall; stand the box at world x = -120,
        // nearer than the +256 far wall, so it occludes geometry behind it.
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);

        let without = render_scene_ext(
            &world, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None,
        );
        let with = render_scene_ext(
            &world,
            &cam,
            160,
            120,
            &pal,
            &[],
            &[],
            &[ExternalBModel { bsp: &box_bsp, origin: [-120.0, 0.0, 0.0] }],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );

        // The box is nearer than the far wall, so drawing it must CHANGE pixels.
        let changed = without
            .rgb
            .iter()
            .zip(with.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "external brush bsp changed no pixels (not drawn)");
    }

    #[test]
    fn external_brush_bsp_origin_shifts_geometry() {
        // The same external box at two different origins must land in different
        // places: rendering it centred vs shifted +Y changes pixels.
        let world = demo_room();
        let box_bsp = tiny_brush_bsp();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);

        let centered = render_scene_ext(
            &world,
            &cam,
            160,
            120,
            &pal,
            &[],
            &[],
            &[ExternalBModel { bsp: &box_bsp, origin: [-120.0, 0.0, 0.0] }],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
        let shifted = render_scene_ext(
            &world,
            &cam,
            160,
            120,
            &pal,
            &[],
            &[],
            &[ExternalBModel { bsp: &box_bsp, origin: [-120.0, 120.0, 0.0] }],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
        let changed = centered
            .rgb
            .iter()
            .zip(shifted.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 0, "moving the external box origin should move its pixels");
    }

    #[test]
    fn empty_external_slice_matches_no_external() {
        // Passing an empty `external` slice must be byte-identical to the prior
        // behaviour (so render_scene and every legacy caller are unchanged).
        let world = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let a = render_scene(&world, &cam, 160, 120, &pal, &[]);
        let b = render_scene_ext(
            &world, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None,
        );
        assert_eq!(a.rgb, b.rgb, "empty external slice must equal render_scene");
    }

    #[test]
    fn external_brush_bsp_empty_or_malformed_is_safe() {
        // A missing box bsp (empty: no models/faces) and one with a corrupt face
        // must both draw nothing and never panic. The empty-bsp render must equal
        // the no-external render; the corrupt-face render must not panic.
        let world = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);

        // An empty bsp (no models, no faces) — like an unparseable box.
        let empty = Bsp {
            version: 29,
            entities: String::new(),
            planes: Vec::new(),
            vertexes: Vec::new(),
            edges: Vec::new(),
            faces: Vec::new(),
            nodes: Vec::new(),
            leafs: Vec::new(),
            clipnodes: Vec::new(),
            texinfo: Vec::new(),
            models: Vec::new(),
            marksurfaces: Vec::new(),
            surfedges: Vec::new(),
            textures: Vec::new(),
            visibility: Vec::new(),
            lighting: Vec::new(),
        };
        let baseline = render_scene_ext(
            &world, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None,
        );
        let with_empty = render_scene_ext(
            &world,
            &cam,
            160,
            120,
            &pal,
            &[],
            &[],
            &[ExternalBModel { bsp: &empty, origin: [-120.0, 0.0, 0.0] }],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
        assert_eq!(
            baseline.rgb, with_empty.rgb,
            "an empty/missing external box must draw nothing"
        );

        // A box whose single face references out-of-range edges/planes/texinfo:
        // the face is skipped, no panic.
        let mut bad = tiny_brush_bsp();
        if let Some(f) = bad.faces.last_mut() {
            f.firstedge = 1_000_000;
            f.planenum = 30_000;
            f.texinfo = 30_000;
        }
        let _ = render_scene_ext(
            &world,
            &cam,
            80,
            60,
            &pal,
            &[],
            &[],
            &[ExternalBModel { bsp: &bad, origin: [-120.0, 0.0, 0.0] }],
            None,
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
    }

    #[test]
    fn draw_brush_bsp_paints_at_projected_location() {
        // The standalone `draw_brush_bsp` entry must paint into a caller-owned
        // image + z-buffer at the box's projected location, and a missing/empty
        // bsp must leave both untouched (no panic).
        let box_bsp = tiny_brush_bsp();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let (w, h) = (160usize, 120usize);
        let bg = [10u8, 10, 14];

        let mut image = Image::new(w, h, bg);
        let mut zbuf = vec![f32::INFINITY; w * h];
        draw_brush_bsp(
            &mut image,
            &mut zbuf,
            &cam,
            &box_bsp,
            [-120.0, 0.0, 0.0],
            &pal,
            0.0,
            &NEUTRAL_LIGHTSTYLE_SCALES,
        );
        let painted = image.rgb.iter().filter(|&&p| p != bg).count();
        assert!(painted > 0, "draw_brush_bsp painted nothing at the box location");
        // Some z-buffer cells must now be finite (depth was written).
        assert!(zbuf.iter().any(|z| z.is_finite()), "draw_brush_bsp wrote no depth");

        // A missing/empty bsp leaves a fresh image untouched and does not panic.
        let empty = {
            let mut e = tiny_brush_bsp();
            e.models.clear();
            e
        };
        let mut image2 = Image::new(w, h, bg);
        let mut zbuf2 = vec![f32::INFINITY; w * h];
        draw_brush_bsp(
            &mut image2,
            &mut zbuf2,
            &cam,
            &empty,
            [-120.0, 0.0, 0.0],
            &pal,
            0.0,
            &NEUTRAL_LIGHTSTYLE_SCALES,
        );
        assert!(
            image2.rgb.iter().all(|&p| p == bg),
            "an empty external box must leave the image untouched"
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
            numtris: 2,
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
            // Two oppositely-wound triangles over the same three verts so the
            // fixture is double-sided. Real weapon models are closed solids that
            // always present a FRONT-facing triangle toward the eye; a lone
            // one-sided triangle is not, and the FIX-3 screen-space backface cull
            // would (correctly) drop it whenever its single winding faces away.
            // The reverse winding keeps the fixture visible from either side,
            // mirroring a real model, so these view-anchoring tests still probe
            // placement rather than an artefact of one-sided test geometry.
            triangles: vec![
                Triangle { facesfront: 1, vertindex: [0, 1, 2] },
                Triangle { facesfront: 1, vertindex: [0, 2, 1] },
            ],
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
        let cam_a = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let cam_b = Camera { pos: [0.0, 0.0, 0.0], yaw: 137.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };

        let img_a = render_scene_ext(
            &bsp, &cam_a, w, h, &pal, &[], &[], &[],
            Some(Viewmodel { mdl: &gun, frame: 0 }),
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
        let img_b = render_scene_ext(
            &bsp, &cam_b, w, h, &pal, &[], &[], &[],
            Some(Viewmodel { mdl: &gun, frame: 0 }),
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
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
        let cam = Camera { pos: [200.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };

        // Sanity: the wall actually fills the view (without the gun).
        let world = render_scene_ext(&bsp, &cam, w, h, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None);
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
            &bsp, &cam, w, h, &pal, &[], &[], &[],
            Some(Viewmodel { mdl: &gun, frame: 0 }),
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
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
        let b = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None);
        assert_eq!(a.rgb, b.rgb, "None viewmodel must equal render_scene");
    }

    #[test]
    fn viewmodel_tolerates_malformed_model() {
        // A weapon model with out-of-range triangle indices and no frames must be
        // skipped without panicking and without altering the frame.
        let bsp = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };

        // Frameless model -> draw_viewmodel returns early.
        let mut frameless = viewmodel_mdl();
        frameless.frames.clear();
        let img = render_scene_ext(
            &bsp, &cam, 80, 60, &pal, &[], &[], &[],
            Some(Viewmodel { mdl: &frameless, frame: 0 }),
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
        let baseline = render_scene_ext(&bsp, &cam, 80, 60, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None);
        assert_eq!(img.rgb, baseline.rgb, "frameless weapon must draw nothing");

        // Out-of-range triangle vertex index -> that triangle is skipped.
        let mut bad = viewmodel_mdl();
        bad.triangles = vec![crate::mdl::Triangle { facesfront: 1, vertindex: [0, 1, 9999] }];
        // Must not panic.
        let _ = render_scene_ext(
            &bsp, &cam, 80, 60, &pal, &[], &[], &[],
            Some(Viewmodel { mdl: &bad, frame: 0 }),
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );
    }

    /// A viewmodel whose geometry deliberately *straddles* the near plane: in
    /// model space its forward axis (`+X`) runs from well behind the eye to well
    /// in front of it, so after the camera anchor + the small `OFS_FORWARD` the
    /// grip end is behind `vz == NEAR` and the barrel end is in front — exactly
    /// the authentic held-gun layout that the near-plane CLIP must handle.
    fn straddling_viewmodel_mdl() -> crate::mdl::Mdl {
        use crate::mdl::{AliasFrame, Frame, Mdl, MdlHeader, Skin, StVert, Triangle, TriVertex};
        let header = MdlHeader {
            ident: i32::from_le_bytes(*b"IDPO"),
            version: 6,
            scale: [1.0, 1.0, 1.0],
            // Model-X (forward) runs from -20 (grip, behind the eye after the
            // small forward offset) to +20 (barrel, in front). Model-Z < 0 keeps
            // it below the eye, like a real weapon.
            scale_origin: [-20.0, 0.0, -8.0],
            boundingradius: 64.0,
            eyeposition: [0.0, 0.0, 0.0],
            numskins: 1,
            skinwidth: 1,
            skinheight: 1,
            numverts: 3,
            numtris: 2,
            numframes: 1,
            synctype: 0,
            flags: 0,
            size: 1.0,
        };
        // Decoded model space: X in [-20, +20] (straddles the eye), Z in [-8, 0].
        let verts = vec![
            TriVertex { v: [0, 0, 0], lightnormalindex: 0 },   // X=-20 (behind)
            TriVertex { v: [40, 0, 0], lightnormalindex: 0 },   // X=+20 (in front)
            TriVertex { v: [20, 0, 8], lightnormalindex: 0 },   // X=0 (on the eye)
        ];
        Mdl {
            header,
            skins: vec![Skin::Single(vec![7])],
            stverts: vec![StVert { onseam: 0, s: 0, t: 0 }; 3],
            // Double-sided (both windings) so a FRONT face always greets the eye,
            // exactly as `viewmodel_mdl`.
            triangles: vec![
                Triangle { facesfront: 1, vertindex: [0, 1, 2] },
                Triangle { facesfront: 1, vertindex: [0, 2, 1] },
            ],
            frames: vec![Frame::Single(AliasFrame {
                name: "v0".into(),
                bboxmin: TriVertex { v: [0, 0, 0], lightnormalindex: 0 },
                bboxmax: TriVertex { v: [40, 0, 8], lightnormalindex: 0 },
                verts,
            })],
        }
    }

    #[test]
    fn viewmodel_straddling_near_plane_is_clipped_not_dropped() {
        // A viewmodel that crosses the near plane (part behind the eye, part in
        // front) must be CLIPPED — its front part still draws SOME pixels — rather
        // than having every crossing triangle dropped whole (the old behaviour,
        // which is exactly why `OFS_FORWARD` used to shove the gun far away). The
        // render must not panic.
        let bsp = demo_room();
        let mut pal = [[0u8; 3]; 256];
        pal[7] = [255, 255, 0]; // the viewmodel's pure-yellow skin (B == 0)
        let bg = [10u8, 10, 14];
        let (w, h) = (160usize, 120usize);
        let gun = straddling_viewmodel_mdl();
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };

        let img = render_scene_ext(
            &bsp, &cam, w, h, &pal, &[], &[], &[],
            Some(Viewmodel { mdl: &gun, frame: 0 }),
            0.0,
            &[],
            &[],
            &NEUTRAL_LIGHTSTYLE_SCALES,
            None,
        );

        // (a) it drew SOME gun pixels (not all-dropped). With the old whole-tri
        // drop, every straddling triangle vanished and this would be zero.
        let gun_pixels = img.rgb.iter().filter(|&&p| is_gun_pixel(p)).count();
        assert!(
            gun_pixels > 0,
            "straddling viewmodel must be clipped and still draw pixels (got {gun_pixels})"
        );

        // (b) the drawn pixels stay on-screen within the frame (the rasteriser
        // clamps to the framebuffer; this just confirms a non-empty drawn bbox).
        assert!(
            drawn_bbox(&img, bg).is_some(),
            "straddling viewmodel produced a visible bounding box"
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
        // R_InitTurb: tab[i] = round(8 + 8*sin(i*2pi/128)) in texels — DC-biased,
        // so the range is [0, 2*AMP]; index masking keeps any integer in range.
        let turb = TurbTable::new();
        let max = *turb.tab.iter().max().unwrap();
        let min = *turb.tab.iter().min().unwrap();
        assert_eq!(max, (2.0 * TURB_AMP) as i32, "peak should be 2*AMP, got {max}");
        assert_eq!(min, 0, "trough should be 0 (DC-biased), got {min}");
        // `at_int` never panics for huge/negative indices and stays in [0,2*AMP].
        for &k in &[0, 127, 128, -1, 1_000_000, -1_000_000] {
            let v = turb.at_int(k);
            assert!((0..=(2.0 * TURB_AMP) as i32).contains(&v), "at_int({k}) = {v} out of range");
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
        // Animated: the time phase (time*SPEED) shifts the table index, so the
        // sampled texel moves between two times.
        assert!(
            (s0 - s1).abs() > 0.5 || (t0 - t1).abs() > 0.5,
            "warp should change the sample between two times: ({s0},{t0}) vs ({s1},{t1})"
        );
        // Bounded: displacement off the floored base texel is the DC-biased table
        // value in [0, 2*AMP] (then rem_euclid wraps it into the 64-texel liquid).
        for (warped, base) in [(s1, s.floor()), (t1, t.floor())] {
            let d = warped - base;
            assert!((0.0..=2.0 * TURB_AMP + 1e-3).contains(&d), "displacement {d} out of range");
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
                None,
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

    /// The brightness -> colormap-row curve must reproduce `R_BuildLightMap`'s
    /// bound/invert/shift exactly (the no-overbright clamp at the top, the
    /// darkest row at the bottom, and the monotone ramp in between).
    #[test]
    fn colormap_row_matches_quake_curve() {
        // brightness 2.0 (the static fullbright ceiling) and anything brighter
        // collapse to the BRIGHTEST row 0 — Quake's `t >= 64` no-overbright clamp.
        assert_eq!(colormap_row(2.0), 0, "fullbright must be row 0 (no overbright)");
        assert_eq!(colormap_row(3.0), 0, "overbright must clamp to row 0");
        assert_eq!(colormap_row(MAX_LIGHT_FACTOR), 0, "max dynamic light clamps to row 0");

        // brightness 0.0 (fully dark) -> the DARKEST row.
        assert_eq!(colormap_row(0.0), COLORMAP_ROWS - 1, "fully dark must be the last row");

        // The middle: brightness 1.0 -> bl = 32640, t = (65280-32640)>>2 = 8160,
        // row = 8160 >> 8 = 31. This is the exact integer C result.
        assert_eq!(colormap_row(1.0), 31, "neutral brightness must hit row 31");

        // Monotonic: brighter never maps to a darker (larger-index) row, and the
        // row stays inside the table for every factor in the legal range.
        let mut prev = COLORMAP_ROWS; // sentinel above the max row
        let mut b = 0.0f32;
        while b <= MAX_LIGHT_FACTOR + 1e-3 {
            let row = colormap_row(b);
            assert!(row < COLORMAP_ROWS, "row {row} out of table at brightness {b}");
            assert!(row <= prev, "brightness {b} -> row {row} not monotone (prev {prev})");
            prev = row;
            b += 0.05;
        }
    }

    /// A mid-shade Normal-surface pixel must route through
    /// `palette[colormap[row*256 + texel]]` (an INDEX lookup) when a colormap is
    /// supplied, and fall back to the linear `palette[texel]*brightness` multiply
    /// when it is `None`.
    #[test]
    fn colormap_routes_normal_pixel_through_index_lookup() {
        // Palette: index i -> grey (i,i,i), so a palette index is recoverable
        // from the written pixel's red channel.
        let mut pal = [[0u8; 3]; 256];
        for (i, p) in pal.iter_mut().enumerate() {
            *p = [i as u8, i as u8, i as u8];
        }

        // A 1x1 texture whose only texel is index 200.
        const TEXEL: u8 = 200;
        let pixels = [TEXEL];

        // Synthetic colormap (64 rows x 256): a known ramp where
        // colormap[row*256 + col] = (col + row) mod 256. Picking row R and
        // col=TEXEL therefore yields palette index (TEXEL + R) mod 256.
        let mut cm = vec![0u8; COLORMAP_LEN];
        for row in 0..COLORMAP_ROWS {
            for col in 0..256usize {
                cm[row * 256 + col] = ((col + row) % 256) as u8;
            }
        }

        // A full-framebuffer triangle at constant (s,t)=(0,0) so every covered
        // pixel samples texel 200. No LightMap -> brightness == `shade`.
        let (w, h) = (8usize, 8usize);
        let shade = 1.0f32; // -> colormap_row(1.0) == 31
        let expected_row = colormap_row(shade);

        let render = |colormap: Option<&[u8]>| {
            let mut img = Image::new(w, h, [0, 0, 0]);
            let mut zb = vec![f32::INFINITY; w * h];
            let v0 = ProjT { x: 0.0, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
            let v1 = ProjT { x: w as f32, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
            let v2 = ProjT { x: 0.0, y: h as f32, vz: 1.0, s: 0.0, t: 0.0 };
            raster_triangle_tex(
                &mut img, &mut zb, v0, v1, v2,
                &pixels, 1, 1, &pal, shade, None, SurfaceMode::Normal,
                colormap,
            );
            img
        };

        // With the colormap: pixel = palette[colormap[row*256 + 200]] where the
        // ramp gives index (200 + row) mod 256, so red channel == that index.
        let with_cm = render(Some(&cm));
        let drawn: Vec<[u8; 3]> = with_cm.rgb.iter().copied().filter(|p| *p != [0, 0, 0]).collect();
        assert!(!drawn.is_empty(), "colormapped triangle drew nothing");
        let want_index = ((TEXEL as usize + expected_row) % 256) as u8;
        for p in &drawn {
            assert_eq!(
                p[0], want_index,
                "colormap path must yield palette index {want_index} (row {expected_row}, texel {TEXEL})"
            );
        }

        // Without the colormap: the legacy linear multiply. shade==1.0 so the
        // pixel is exactly palette[200] = (200,200,200) (no darkening).
        let without_cm = render(None);
        let drawn2: Vec<[u8; 3]> = without_cm.rgb.iter().copied().filter(|p| *p != [0, 0, 0]).collect();
        assert!(!drawn2.is_empty(), "fallback triangle drew nothing");
        for p in &drawn2 {
            assert_eq!(*p, [TEXEL, TEXEL, TEXEL], "None path must be the linear palette[texel]*brightness");
        }

        // Sanity: the two paths actually differ (the colormap is doing work).
        assert_ne!(want_index, TEXEL, "test ramp should remap the index at row 31");
    }

    /// Liquids/sky stay fullbright = the brightest row 0 (`colormap[texel]`) even
    /// when their `brightness` is the neutral 1.0 — the row is forced to 0 by the
    /// surface mode, not derived from the brightness.
    #[test]
    fn colormap_fullbright_surfaces_use_row_zero() {
        let mut pal = [[0u8; 3]; 256];
        for (i, p) in pal.iter_mut().enumerate() {
            *p = [i as u8, i as u8, i as u8];
        }
        const TEXEL: u8 = 77;
        // 64x64 so the Turb warp's index wrap is well-defined; fill with TEXEL.
        let pixels = vec![TEXEL; 64 * 64];

        // Colormap ramp: colormap[row*256+col] = (col + row) mod 256. Row 0 keeps
        // the index unchanged, so a fullbright (row-0) pixel == palette[TEXEL].
        let mut cm = vec![0u8; COLORMAP_LEN];
        for row in 0..COLORMAP_ROWS {
            for col in 0..256usize {
                cm[row * 256 + col] = ((col + row) % 256) as u8;
            }
        }

        let turb = TurbTable::new();
        let (w, h) = (16usize, 16usize);
        let mut img = Image::new(w, h, [0, 0, 0]);
        let mut zb = vec![f32::INFINITY; w * h];
        let v0 = ProjT { x: 0.0, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
        let v1 = ProjT { x: w as f32, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
        let v2 = ProjT { x: 0.0, y: h as f32, vz: 1.0, s: 0.0, t: 0.0 };
        raster_triangle_tex(
            &mut img, &mut zb, v0, v1, v2,
            &pixels, 64, 64, &pal, 1.0, None,
            SurfaceMode::Turb { turb: &turb, time: 0.0 },
            Some(&cm),
        );
        let drawn: Vec<[u8; 3]> = img.rgb.iter().copied().filter(|p| *p != [0, 0, 0]).collect();
        assert!(!drawn.is_empty(), "fullbright triangle drew nothing");
        for p in &drawn {
            // Row 0: index unchanged -> palette[TEXEL] grey.
            assert_eq!(*p, [TEXEL, TEXEL, TEXEL], "fullbright surface must use colormap row 0");
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
        // pixels — i.e. show the sky texture, not a flat fill — (2) differ between
        // two times (it scrolls), and (3) differ when the VIEW DIRECTION changes
        // (the sky is projected from the view ray, D_Sky_uv_To_st — not the wall
        // (s,t)).
        let pixels = synthetic_sky_pixels();
        let mut pal = [[0u8; 3]; 256];
        for (i, p) in pal.iter_mut().enumerate() {
            // Map every index to a distinct, clearly non-zero colour so any
            // sampled sky texel is visibly different from the [0,0,0] background.
            *p = [(i as u8).max(1), 255u8.saturating_sub(i as u8), 128];
        }

        let (w, h) = (48usize, 48usize);
        // Build a SkyView for a given look direction (forward), with an orthonormal
        // right/up basis. This stands in for the camera the world pass passes in.
        let make_view = |forward: Vec3| {
            let (f, _) = normalize(forward);
            // right = forward x worldup, up = right x forward (orthonormal-ish).
            let (right, _) = normalize(cross(f, [0.0, 0.0, 1.0]));
            let (up, _) = normalize(cross(right, f));
            SkyView {
                forward: f,
                right,
                up,
                cx: w as f32 / 2.0,
                cy: h as f32 / 2.0,
                longest: w.max(h) as f32,
            }
        };
        let render_at = |time: f32, view: SkyView| {
            let mut img = Image::new(w, h, [0, 0, 0]); // background = pure black
            let mut zb = vec![f32::INFINITY; w * h];
            // The (s,t) here are IGNORED by the sky path (it uses the view ray),
            // but a covering triangle is still needed to rasterise the screen area.
            let v0 = ProjT { x: 0.0, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
            let v1 = ProjT { x: w as f32, y: 0.0, vz: 1.0, s: 0.0, t: 0.0 };
            let v2 = ProjT { x: 0.0, y: h as f32, vz: 1.0, s: 0.0, t: 0.0 };
            raster_triangle_tex(
                &mut img, &mut zb, v0, v1, v2,
                &pixels, 256, 128, &pal, 1.0, None,
                SurfaceMode::Sky { time, view },
                None,
            );
            img
        };
        let view_n = make_view([1.0, 0.0, 0.0]); // looking +X
        let a = render_at(0.0, view_n);
        let b = render_at(1.0, view_n);

        // (1) Non-background: the sky drew real texels (not a flat empty frame).
        let drawn = a.rgb.iter().filter(|&&p| p != [0, 0, 0]).count();
        assert!(drawn > 0, "sky face rendered no pixels (should show the sky texture)");

        // (2) Animated: scrolling shifts the texels, so the two frames differ.
        let changed = a.rgb.iter().zip(b.rgb.iter()).filter(|(x, y)| x != y).count();
        assert!(changed > 0, "sky must scroll (differ) between two times");

        // (3) View-dependent: looking a different direction shows a different patch
        // of sky (the whole point of projecting the view ray).
        let view_e = make_view([0.0, 1.0, 0.0]); // looking +Y
        let c = render_at(0.0, view_e);
        let view_diff = a.rgb.iter().zip(c.rgb.iter()).filter(|(x, y)| x != y).count();
        assert!(view_diff > 0, "sky must change with the view direction (dome projection)");
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

        let a = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None);
        let b = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], &[], None, 0.6, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None);

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
        let t0 = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None);
        let t1 = render_scene_ext(&bsp, &cam, 160, 120, &pal, &[], &[], &[], None, 9.5, &[], &[], &NEUTRAL_LIGHTSTYLE_SCALES, None);
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
            anim: None,
        };
        let sky = MipTex {
            name: "sky1".into(),
            width: 256,
            height: 128,
            offsets: [0, 0, 0, 0],
            pixels: synthetic_sky_pixels(),
            anim: None,
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

    /// A test conchars atlas where every glyph texel is the lit index 3 (except
    /// the byte-0 cell, which stays the transparent index 0), so any drawn
    /// label/value paints index-3 pixels.
    fn test_conchars() -> Qpic {
        let mut data = vec![3u8; 128 * 128];
        for y in 0..8 {
            for x in 0..8 {
                data[y * 128 + x] = 0;
            }
        }
        Qpic { width: 128, height: 128, data }
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
    /// and never transparent), `anum_0..anum_9` (24x24, index `120+d`), and the
    /// intermission `num_colon`/`num_slash`/`num_minus` (index 140/141/142).
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
        pics.push(("num_colon".to_string(), qpic_payload(16, 24, 140)));
        pics.push(("num_slash".to_string(), qpic_payload(16, 24, 141)));
        pics.push(("num_minus".to_string(), qpic_payload(16, 24, 142)));

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
        let hud = Hud {
            wad: &wad,
            palette: &pal,
            health: 100,
            ammo: 25,
            armor: 50,
            items: 0,
            weapon: 0,
            ammo_shells: 0,
            ammo_nails: 0,
            ammo_rockets: 0,
            ammo_cells: 0,
            time: 0.0,
            monsters: 0,
            total_monsters: 0,
            secrets: 0,
            total_secrets: 0,
            level_name: "",
            show_scores: false,
            sb_lines: SB_LINES_FULL,
        };
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
        let hud = Hud {
            wad: &wad,
            palette: &pal,
            health: 99,
            ammo: 100,
            armor: 0,
            items: 0,
            weapon: 0,
            ammo_shells: 0,
            ammo_nails: 0,
            ammo_rockets: 0,
            ammo_cells: 0,
            time: 0.0,
            monsters: 0,
            total_monsters: 0,
            secrets: 0,
            total_secrets: 0,
            level_name: "",
            show_scores: false,
            sb_lines: SB_LINES_FULL,
        };
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
        let hud = Hud {
            wad: &wad,
            palette: &pal,
            health: 100,
            ammo: 50,
            armor: 25,
            items: 0,
            weapon: 0,
            ammo_shells: 0,
            ammo_nails: 0,
            ammo_rockets: 0,
            ammo_cells: 0,
            time: 0.0,
            monsters: 0,
            total_monsters: 0,
            secrets: 0,
            total_secrets: 0,
            level_name: "",
            show_scores: false,
            sb_lines: SB_LINES_FULL,
        };
        draw_hud_into(&mut img, &hud);
        assert!(img.rgb.iter().all(|&p| p == fill), "missing pics leave the frame unchanged");
    }

    // -- Inventory bar / face / icons (Sbar_DrawInventory + Sbar_DrawFace) -----

    #[test]
    fn face_bracket_matches_sbar_drawface() {
        // Sbar_DrawFace: health >= 100 -> 4; else health/20 (int div), clamped 0..4.
        assert_eq!(face_bracket(0), 0); // dead-ish -> lowest (face5)
        assert_eq!(face_bracket(19), 0);
        assert_eq!(face_bracket(20), 1);
        assert_eq!(face_bracket(39), 1);
        assert_eq!(face_bracket(40), 2);
        assert_eq!(face_bracket(59), 2);
        assert_eq!(face_bracket(60), 3);
        assert_eq!(face_bracket(79), 3);
        assert_eq!(face_bracket(80), 4);
        assert_eq!(face_bracket(99), 4);
        assert_eq!(face_bracket(100), 4); // full health (face1)
        assert_eq!(face_bracket(250), 4); // mega-health still clamps to 4
        assert_eq!(face_bracket(-50), 0); // never panics / never out of range
        // The bracket indexes the FACE_NAMES table (face5..face1).
        assert_eq!(FACE_NAMES[face_bracket(100)], "face1");
        assert_eq!(FACE_NAMES[face_bracket(10)], "face5");
    }

    #[test]
    fn weapon_flash_name_cycles_five_frames() {
        // The selected-weapon flash cycles inva1..inva5 off (int)(time*10) % 5.
        assert_eq!(weapon_flash_name(0, 0.0), "inva1_shotgun");
        assert_eq!(weapon_flash_name(0, 0.1), "inva2_shotgun");
        assert_eq!(weapon_flash_name(0, 0.4), "inva5_shotgun");
        assert_eq!(weapon_flash_name(0, 0.5), "inva1_shotgun"); // wraps after 5
        // Per-weapon suffix is correct across the 7 weapons (shotgun..lightng).
        assert_eq!(weapon_flash_name(6, 0.0), "inva1_lightng");
        assert_eq!(weapon_flash_name(4, 0.0), "inva1_rlaunch");
    }

    /// A 128x128 conchars atlas with EVERY glyph cell solidly filled (index 95),
    /// so any drawn character paints recognisable pixels.
    fn solid_conchars() -> Qpic {
        Qpic { width: 128, height: 128, data: vec![95u8; 128 * 128] }
    }

    #[test]
    fn intermission_overlay_draws_banner_plaque_and_numbers() {
        // Sbar_IntermissionOverlay on a 320x200 frame (scale 1, no offsets): the
        // banner at (64,24), the plaque at (0,56), and the verbatim sbar.c number
        // positions — minutes right-justified into 3 slots from x=160, colon at
        // 234, second digits at 246/266; secrets/monsters rows at y=104/144 with
        // num_slash at 232 and the totals from x=240.
        let pal = ramp_palette();
        let wad = build_hud_wad();
        let mut img = Image::new(320, 200, [0, 0, 0]);
        let complete = Qpic { width: 192, height: 24, data: vec![50u8; 192 * 24] };
        let inter = Qpic { width: 160, height: 144, data: vec![51u8; 160 * 144] };
        let stats = IntermissionStats {
            completed_time: 205, // 3:25
            secrets: 3,
            total_secrets: 7,
            monsters: 12,
            total_monsters: 45,
        };
        draw_intermission_overlay(&mut img, &wad, &pal, Some(&complete), Some(&inter), &stats);

        let px = |x: usize, y: usize| img.rgb[y * 320 + x];
        assert_eq!(px(64 + 5, 24 + 5), [50, 50, 50], "complete.lmp banner at (64,24)");
        assert_eq!(px(5, 56 + 100), [51, 51, 51], "inter.lmp plaque at (0,56)");
        // Time 3:25 — "3" right-justified: x = 160 + 2*24 = 208 (num_3 = idx 103).
        assert_eq!(px(208 + 2, 64 + 2), [103, 103, 103], "minutes digit 3 at x=208");
        assert_eq!(px(234 + 2, 64 + 2), [140, 140, 140], "num_colon at x=234");
        assert_eq!(px(246 + 2, 64 + 2), [102, 102, 102], "seconds tens 2 at x=246");
        assert_eq!(px(266 + 2, 64 + 2), [105, 105, 105], "seconds units 5 at x=266");
        // Secrets 3/7: found at x=208 (right-justified), slash 232, total at 288.
        assert_eq!(px(208 + 2, 104 + 2), [103, 103, 103], "secrets found 3");
        assert_eq!(px(232 + 2, 104 + 2), [141, 141, 141], "num_slash at x=232");
        assert_eq!(px(240 + 2 * 24 + 2, 104 + 2), [107, 107, 107], "secrets total 7");
        // Monsters 12/45: two digits start at x = 160 + 24 = 184.
        assert_eq!(px(184 + 2, 144 + 2), [101, 101, 101], "monsters tens 1 at x=184");
        assert_eq!(px(208 + 2, 144 + 2), [102, 102, 102], "monsters units 2 at x=208");
        assert_eq!(px(264 + 2, 144 + 2), [104, 104, 104], "total tens 4 at x=264");
        assert_eq!(px(288 + 2, 144 + 2), [105, 105, 105], "total units 5 at x=288");
        // The (missing-pic) graceful path: no panic with both pics absent.
        let mut img2 = Image::new(320, 200, [0, 0, 0]);
        draw_intermission_overlay(&mut img2, &wad, &pal, None, None, &stats);
        assert_eq!(px(208 + 2, 64 + 2), [103, 103, 103], "numbers still draw without pics");
    }

    #[test]
    fn finale_center_string_reveals_at_printspeed() {
        // SCR_DrawCenterString's finale reveal: remaining = 8 * elapsed, and the
        // post-decrement `if (!remaining--) return;` paints remaining+1 chars.
        let pal = ramp_palette();
        let cc = solid_conchars();
        // "AB\nCD": 2 lines (<= 4) so the block starts at y = 200*0.35 = 70; each
        // 2-char line centers at vx = (320 - 16)/2 = 152.
        let text = "AB\nCD";

        // elapsed 0 -> remaining 0 -> exactly ONE char ('A') is painted.
        let mut img = Image::new(320, 200, [0, 0, 0]);
        draw_finale_overlay(&mut img, Some(&cc), &pal, None, text, 0.0);
        let px = |img: &Image, x: usize, y: usize| img.rgb[y * 320 + x];
        assert_eq!(px(&img, 152 + 1, 70 + 1), [95, 95, 95], "first char visible at once");
        assert_eq!(px(&img, 160 + 1, 70 + 1), [0, 0, 0], "second char not yet revealed");
        assert_eq!(px(&img, 152 + 1, 78 + 1), [0, 0, 0], "second line not yet revealed");

        // elapsed 1s -> remaining 8 -> all four chars painted (budget exceeds text).
        let mut img = Image::new(320, 200, [0, 0, 0]);
        draw_finale_overlay(&mut img, Some(&cc), &pal, None, text, 1.0);
        assert_eq!(px(&img, 160 + 1, 70 + 1), [95, 95, 95], "line 1 fully revealed");
        assert_eq!(px(&img, 160 + 1, 78 + 1), [95, 95, 95], "line 2 fully revealed");

        // The finale plaque centers horizontally at y=16 (Sbar_FinaleOverlay).
        let plaque = Qpic { width: 100, height: 20, data: vec![52u8; 100 * 20] };
        let mut img = Image::new(320, 200, [0, 0, 0]);
        draw_finale_overlay(&mut img, Some(&cc), &pal, Some(&plaque), "", 0.0);
        assert_eq!(px(&img, 110 + 1, 16 + 1), [52, 52, 52], "finale.lmp centered at y=16");
        assert_eq!(px(&img, 100, 16 + 1), [0, 0, 0], "left of the centered plaque is clear");
    }

    #[test]
    fn negative_reveal_budget_paints_the_whole_string() {
        // SCR_DrawCenterString: `if (!remaining--) return;` only fires when
        // `remaining` is exactly 0 at the check — a budget that STARTS below
        // zero keeps decrementing past it and paints the WHOLE string. (An
        // early `remaining < 0 => return` would paint nothing.)
        let pal = ramp_palette();
        let cc = solid_conchars();
        let px = |img: &Image, x: usize, y: usize| img.rgb[y * 320 + x];

        let mut img = Image::new(320, 200, [0, 0, 0]);
        draw_center_string_revealed(&mut img, &cc, &pal, "AB\nCD", -1);
        assert_eq!(px(&img, 160 + 1, 70 + 1), [95, 95, 95], "line 1 fully painted");
        assert_eq!(px(&img, 160 + 1, 78 + 1), [95, 95, 95], "line 2 fully painted");

        // i32::MIN paints everything too (and the decrement must not panic).
        let mut img = Image::new(320, 200, [0, 0, 0]);
        draw_center_string_revealed(&mut img, &cc, &pal, "AB\nCD", i32::MIN);
        assert_eq!(px(&img, 160 + 1, 78 + 1), [95, 95, 95], "i32::MIN = unlimited");
    }

    #[test]
    fn intermission_number_draws_leading_minus_glyph() {
        // Sbar_IntermissionNumber: Sbar_itoa keeps the sign, and a '-' draws
        // sb_nums[0][STAT_MINUS] — "num_minus" (index 142 in the test wad). -7
        // over 3 slots right-justifies like any 2-character number: the minus
        // lands at x = 160 + 24 = 184 and the digit at 208.
        let pal = ramp_palette();
        let wad = build_hud_wad();
        let mut img = Image::new(320, 200, [0, 0, 0]);
        intermission_number(&mut img, &wad, &pal, -7, 160.0, 64.0, 3, 1.0, 0.0, 0.0);
        let px = |x: usize, y: usize| img.rgb[y * 320 + x];
        assert_eq!(px(160 + 2, 64 + 2), [0, 0, 0], "first slot empty (2-char number)");
        assert_eq!(px(184 + 2, 64 + 2), [142, 142, 142], "minus glyph at x=184");
        assert_eq!(px(208 + 2, 64 + 2), [107, 107, 107], "digit 7 at x=208");
    }

    /// A fuller synthetic `gfx.wad` adding the inventory-bar art the base
    /// `build_hud_wad` omits: `ibar` (320x24, index 2), the 7 `inv_*` weapon icons +
    /// the 35 `inva{1..5}_*` flash icons (24x16, index 60), the 5 health faces +
    /// 4 powerup faces (24x24, index 70), the armour/ammo-type icons (24x24, index
    /// 80/85), the key/powerup/sigil item icons, and a 128x128 `conchars` whose
    /// gold-digit cells (18..27) are non-zero so the ammo counts render.
    fn build_full_hud_wad() -> Wad2 {
        let mut pics: Vec<(String, Vec<u8>)> = Vec::new();
        pics.push(("sbar".to_string(), qpic_payload(320, 24, 1)));
        pics.push(("ibar".to_string(), qpic_payload(320, 24, 2)));
        for d in 0..10u8 {
            pics.push((format!("num_{d}"), qpic_payload(24, 24, 100 + d)));
        }
        for d in 0..10u8 {
            pics.push((format!("anum_{d}"), qpic_payload(24, 24, 120 + d)));
        }
        // Weapon icons (dim inv_*, bright active inv2_*, and the 5 flash frames),
        // distinct index 60 so they show.
        for s in WEAPON_SUFFIX {
            pics.push((format!("inv_{s}"), qpic_payload(24, 16, 60)));
            pics.push((format!("inv2_{s}"), qpic_payload(24, 16, 60)));
            for f in 1..=5u8 {
                pics.push((format!("inva{f}_{s}"), qpic_payload(24, 16, 60)));
            }
        }
        // Faces (health brackets + powerups), index 70.
        for name in ["face5", "face4", "face3", "face2", "face1", "face_inv2", "face_quad", "face_invis", "face_invul2"] {
            pics.push((name.to_string(), qpic_payload(24, 24, 70)));
        }
        // Armour-type + ammo-type icons, index 80 / 85.
        for name in ARMOR_ICON_NAMES {
            pics.push((name.to_string(), qpic_payload(24, 24, 80)));
        }
        for name in AMMO_ICON_NAMES {
            pics.push((name.to_string(), qpic_payload(24, 24, 85)));
        }
        // Keys / powerups / sigils, index 90.
        for name in SB_ITEM_NAMES {
            pics.push((name.to_string(), qpic_payload(16, 16, 90)));
        }
        for name in SB_SIGIL_NAMES {
            pics.push((name.to_string(), qpic_payload(8, 16, 90)));
        }
        // conchars: a raw 128x128 byte block (NO qpic header) — the gold digit
        // cells 18..27 (rows 1, cols 2..11) set to index 95 so the small ammo
        // counts draw a recognisable colour.
        let mut conchars_raw = vec![0u8; 128 * 128];
        for cell in 18..=27usize {
            let cx = (cell % 16) * 8;
            let cy = (cell / 16) * 8;
            for gy in 0..8 {
                for gx in 0..8 {
                    conchars_raw[(cy + gy) * 128 + (cx + gx)] = 95;
                }
            }
        }

        // Lay payloads after the 12-byte header. `conchars` is appended RAW (no
        // qpic_payload header) and registered as a non-QPIC lump via push_raw_lump.
        let mut payloads = Vec::new();
        let mut offsets = Vec::new();
        let mut pos = WADINFO_SIZE;
        for (_, p) in &pics {
            offsets.push(pos);
            payloads.extend_from_slice(p);
            pos += p.len();
        }
        let conchars_off = pos;
        payloads.extend_from_slice(&conchars_raw);
        pos += conchars_raw.len();
        let infotableofs = pos;

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"WAD2");
        bytes.extend_from_slice(&((pics.len() + 1) as i32).to_le_bytes());
        bytes.extend_from_slice(&(infotableofs as i32).to_le_bytes());
        bytes.extend_from_slice(&payloads);

        let mut dir = Vec::new();
        for ((name, p), &off) in pics.iter().zip(offsets.iter()) {
            push_lump(&mut dir, off as i32, p.len() as i32, name);
        }
        // conchars as a raw lump (its type does not matter — lump_data reads bytes).
        push_lump(&mut dir, conchars_off as i32, conchars_raw.len() as i32, "conchars");
        bytes.extend_from_slice(&dir);

        Wad2::parse(bytes).expect("synthetic full gfx.wad parses")
    }

    #[test]
    fn draw_hud_inventory_bar_draws_above_sbar() {
        // With the full wad, the ibar (index 2) must fill the 24 virtual rows ABOVE
        // the 24-row sbar; a face must draw on the sbar; and the inventory elements
        // must appear at their sbar.c positions.
        let wad = build_full_hud_wad();
        let pal = ramp_palette();
        let fill = [42u8, 42, 42];
        let mut img = Image::new(320, 200, fill); // scale 1: bar = bottom 48 rows
        let hud = Hud {
            wad: &wad,
            palette: &pal,
            health: 100,   // -> face1 (bracket 4)
            ammo: 25,
            armor: 80,
            // shotgun(1) + nailgun(4) owned; armour3; shells ammo type;
            // key1 (bit 17) + quad (bit 22); sigil1 (bit 28).
            items: IT_SHOTGUN | (IT_SHOTGUN << 2) | IT_ARMOR3 | IT_SHELLS
                | (1 << 17) | (1 << 22) | (1 << 28),
            weapon: IT_SHOTGUN, // shotgun selected -> flashes inva*_shotgun
            ammo_shells: 100,
            ammo_nails: 0,
            ammo_rockets: 0,
            ammo_cells: 0,
            time: 0.0,
            monsters: 0,
            total_monsters: 0,
            secrets: 0,
            total_secrets: 0,
            level_name: "",
            show_scores: false,
            sb_lines: SB_LINES_FULL,
        };
        draw_hud_into(&mut img, &hud);

        // The ibar (index 2) occupies virtual rows -24..0, i.e. framebuffer rows
        // 152..176 at scale 1. Its background colour [2,2,2] must appear there.
        let ibar_rows = 152..176;
        let ibar_bg = ibar_rows
            .clone()
            .flat_map(|y| (0..320).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 320 + x] == [2, 2, 2])
            .count();
        assert!(ibar_bg > 0, "ibar background fills the 24 rows above the sbar");

        // Rows above the whole 48-row status area (y < 152) stay the fill colour.
        for y in 0..152 {
            for x in 0..320 {
                assert_eq!(img.rgb[y * 320 + x], fill, "row {y} col {x} above the status area untouched");
            }
        }

        // A weapon icon (index 60) drew on the ibar (the active shotgun's bright
        // inv2_shotgun icon at x=0, y=-16 -> framebuffer rows ~160..176).
        let weapon_px = (160..176)
            .flat_map(|y| (0..24).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 320 + x] == [60, 60, 60])
            .count();
        assert!(weapon_px > 0, "active weapon (inv2_*) icon drew on the ibar");

        // The face (index 70) drew at x=112 on the sbar (rows 176..200).
        let face_px = (176..200)
            .flat_map(|y| (112..136).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 320 + x] == [70, 70, 70])
            .count();
        assert!(face_px > 0, "player face drew at x=112 on the sbar");

        // The armour-type icon (index 80) drew at x=0 on the sbar.
        let armor_icon = (176..200)
            .flat_map(|y| (0..24).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 320 + x] == [80, 80, 80])
            .count();
        assert!(armor_icon > 0, "armour-type icon drew at x=0");

        // The ammo-type icon (index 85) drew at x=224 on the sbar.
        let ammo_icon = (176..200)
            .flat_map(|y| (224..248).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 320 + x] == [85, 85, 85])
            .count();
        assert!(ammo_icon > 0, "ammo-type icon drew at x=224");

        // The small ammo counts (gold conchars digits, index 95) drew on the ibar.
        let count_px = ibar_rows
            .flat_map(|y| (0..320).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 320 + x] == [95, 95, 95])
            .count();
        assert!(count_px > 0, "small ammo counts drew on the ibar");

        // A sigil (index 90) drew near the right edge of the ibar (x≈288).
        let sigil_px = (160..176)
            .flat_map(|y| (288..296).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 320 + x] == [90, 90, 90])
            .count();
        assert!(sigil_px > 0, "sigil drew near the right edge of the ibar");
    }

    #[test]
    fn draw_hud_powerup_face_overrides_health_face() {
        // With quad active, Sbar_DrawFace draws the quad face regardless of health.
        let wad = build_full_hud_wad();
        let pal = ramp_palette();
        let mut img = Image::new(320, 200, [0u8, 0, 0]);
        let hud = Hud {
            wad: &wad,
            palette: &pal,
            health: 100,
            ammo: 0,
            armor: 0,
            items: IT_QUAD,
            weapon: 0,
            ammo_shells: 0,
            ammo_nails: 0,
            ammo_rockets: 0,
            ammo_cells: 0,
            time: 0.0,
            monsters: 0,
            total_monsters: 0,
            secrets: 0,
            total_secrets: 0,
            level_name: "",
            show_scores: false,
            sb_lines: SB_LINES_FULL,
        };
        // All face pics share index 70 here, so we can't distinguish quad vs health
        // by colour — instead assert the call path doesn't panic and a face drew.
        draw_hud_into(&mut img, &hud);
        let face_px = (176..200)
            .flat_map(|y| (112..136).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 320 + x] == [70, 70, 70])
            .count();
        assert!(face_px > 0, "a powerup (quad) face drew at x=112");
    }

    // -- Engine particles (draw_particles: projection + z-test) ---------------

    #[test]
    fn draw_particles_in_front_changes_a_pixel() {
        // A camera at the origin looking down +X; a particle 100 units straight
        // ahead must project near screen centre and paint its palette colour over
        // a fresh (cleared) z-buffer.
        let w = 80usize;
        let h = 60usize;
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
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

    /// Build a trivial single-frame sprite: `w`x`h` pixels all set to `fill`, with a
    /// centred origin so the billboard straddles the projected point.
    #[cfg(test)]
    fn test_sprite(wpx: i32, hpx: i32, fill: u8) -> crate::spr::Sprite {
        use crate::spr::{Frame, Sprite, SpriteFrame, SpriteHeader};
        Sprite {
            header: SpriteHeader {
                ident: 0, version: 1, type_: 0, boundingradius: 0.0,
                width: wpx, height: hpx, numframes: 1, beamlength: 0.0, synctype: 0,
            },
            frames: vec![Frame::Single(SpriteFrame {
                origin: [-wpx / 2, hpx / 2], // centred
                width: wpx, height: hpx,
                pixels: vec![fill; (wpx * hpx) as usize],
            })],
        }
    }

    #[test]
    fn draw_sprite_in_front_paints_and_z_writes() {
        // A facing sprite 100 units ahead projects near centre and paints its colour.
        let (w, h) = (80usize, 60usize);
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let bg = [9u8, 9, 9];
        let mut img = Image::new(w, h, bg);
        let mut zbuf = vec![f32::INFINITY; w * h];
        let mut pal = [[0u8, 0, 0]; 256];
        pal[42] = [200, 50, 30];
        let spr = test_sprite(16, 16, 42);
        let inst = SpriteInstance { sprite: &spr, origin: [100.0, 0.0, 0.0], frame: 0 };
        draw_sprites(&mut img, &mut zbuf, &cam, std::slice::from_ref(&inst), &pal, 0.0, w, h);
        let painted = img.rgb.iter().filter(|&&p| p == [200, 50, 30]).count();
        assert!(painted > 0, "a sprite in front must paint pixels");
        let nearest = zbuf.iter().cloned().fold(f32::INFINITY, f32::min);
        assert!((nearest - 100.0).abs() < 1.0, "z holds the sprite depth, got {nearest}");
    }

    #[test]
    fn draw_sprite_index_255_is_transparent() {
        // An all-255 sprite is fully transparent: nothing is painted.
        let (w, h) = (80usize, 60usize);
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let bg = [9u8, 9, 9];
        let mut img = Image::new(w, h, bg);
        let mut zbuf = vec![f32::INFINITY; w * h];
        let pal = [[7u8, 7, 7]; 256];
        let spr = test_sprite(16, 16, 255);
        let inst = SpriteInstance { sprite: &spr, origin: [100.0, 0.0, 0.0], frame: 0 };
        draw_sprites(&mut img, &mut zbuf, &cam, std::slice::from_ref(&inst), &pal, 0.0, w, h);
        assert!(img.rgb.iter().all(|&p| p == bg), "index-255 texels are transparent (nothing painted)");
        assert!(zbuf.iter().all(|&z| z == f32::INFINITY), "transparent sprite writes no depth");
    }

    #[test]
    fn draw_sprite_behind_wall_is_z_tested_out() {
        // A sprite at depth 100 behind a wall (z-buffer pre-filled to 10) is hidden.
        let (w, h) = (80usize, 60usize);
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let bg = [9u8, 9, 9];
        let mut img = Image::new(w, h, bg);
        let mut zbuf = vec![10.0f32; w * h];
        let mut pal = [[0u8, 0, 0]; 256];
        pal[42] = [200, 50, 30];
        let spr = test_sprite(16, 16, 42);
        let inst = SpriteInstance { sprite: &spr, origin: [100.0, 0.0, 0.0], frame: 0 };
        draw_sprites(&mut img, &mut zbuf, &cam, std::slice::from_ref(&inst), &pal, 0.0, w, h);
        assert!(img.rgb.iter().all(|&p| p == bg), "a sprite behind a nearer wall is z-tested out");
    }

    #[test]
    fn draw_particles_behind_wall_is_z_tested_out() {
        // Same view, but pre-fill the z-buffer with a NEARER depth (a wall at
        // depth 10) everywhere. A particle at depth 100 is behind it and must NOT
        // be drawn (the z-test rejects vz >= zbuf).
        let w = 80usize;
        let h = 60usize;
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
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
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
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
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
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

        // Quit (item 4 on Main) raises the confirm prompt (does NOT close yet).
        m.open();
        m.cursor = 4;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Quit, "Quit raises the confirm prompt");
        assert!(m.visible);
        // Escape ("No") backs out to the screen the prompt rose from (Main here).
        assert_eq!(m.cancel(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Main);
        assert!(m.visible);
        // Re-raise it and answer "Yes" via select (Enter): closes the menu.
        m.cursor = 4;
        m.select();
        assert_eq!(m.screen(), MenuScreen::Quit);
        assert_eq!(m.select(), MenuAction::Closed, "Enter on the Quit prompt quits");
        assert!(!m.visible);

        // Main item Multiplayer (item 1) opens the multiplayer screen
        // (M_Menu_MultiPlayer_f); Enter there does nothing (no net drivers,
        // like the C), and Escape returns to Main.
        m.open();
        m.cursor = 1;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Multiplayer, "item 1 enters Multiplayer");
        assert!(m.visible);
        assert_eq!(m.select(), MenuAction::None, "Join responds with no action (no net)");
        assert_eq!(m.screen(), MenuScreen::Multiplayer);
        assert_eq!(m.cancel(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Main);

        // Help (item 3) now opens the Help screen.
        m.cursor = 3;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Help, "item 3 enters Help");
        assert_eq!(m.help_page(), 0, "Help opens on page 0");
        assert!(m.visible);
        // Escape backs out of Help to Main.
        assert_eq!(m.cancel(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Main);

        // Main item 2 (Options) switches to the Options screen.
        m.open();
        m.cursor = 2;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Options, "item 2 enters Options");
        assert!(m.visible);
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
        draw_menu(&mut img, &m, &pics, None, 0.3, 0.0, &pal);
        assert_eq!(img.rgb, before, "an all-empty MenuPics must leave the frame untouched");

        // A hidden menu never draws.
        m.close();
        let solid = solid_pic(64, 16, 7);
        let pics2 = MenuPics { mainmenu: Some(solid), ..Default::default() };
        draw_menu(&mut img, &m, &pics2, None, 0.3, 0.0, &pal);
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
        draw_menu(&mut img, &m, &pics, None, 0.0, 0.0, &pal);
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
        draw_menu(&mut img0, &m, &pics, None, 0.0, 0.0, &pal); // frame 0 -> index 10
        assert_eq!(img0.rgb[cursor_idx], pal[10]);

        let mut img1 = Image::new(320, 200, [0, 0, 0]);
        draw_menu(&mut img1, &m, &pics, None, 0.35, 0.0, &pal); // (3.5)->3 -> index 13
        assert_eq!(img1.rgb[cursor_idx], pal[13]);
        // The spinner runs on host_time ONLY: realtime moving on (the flashing
        // cursors' clock) leaves the menudot frame alone.
        let mut img2 = Image::new(320, 200, [0, 0, 0]);
        draw_menu(&mut img2, &m, &pics, None, 0.35, 7.3, &pal);
        assert_eq!(img2.rgb[cursor_idx], pal[13], "menudot ignores realtime");
    }

    #[test]
    fn menu_cursor_flashes_at_4hz_on_realtime() {
        // M_Options_Draw & co: 12 + ((int)(realtime*4) & 1). Glyph 12 (blank)
        // for the first quarter second, 13 (the arrow) for the next, and so on
        // — 4 toggles per second, NOT the menudot's 10 Hz frame parity (the old
        // bug: the cursor followed (int)(host_time*10) % 6 & 1, 2.5x too fast).
        assert_eq!(menu_cursor_glyph(0.0), 12);
        assert_eq!(menu_cursor_glyph(0.10), 12, "0.10 s: the 10 Hz parity would say 13");
        assert_eq!(menu_cursor_glyph(0.24), 12);
        assert_eq!(menu_cursor_glyph(0.25), 13);
        assert_eq!(menu_cursor_glyph(0.49), 13);
        assert_eq!(menu_cursor_glyph(0.50), 12);
        assert_eq!(menu_cursor_glyph(0.75), 13);
        // Count the toggles over one second sampled at 1 ms: exactly 4 edges
        // (at 0.25/0.5/0.75/1.0), whatever the frame rate.
        let mut edges = 0;
        let mut prev = menu_cursor_glyph(0.0);
        for ms in 1..=1000 {
            let g = menu_cursor_glyph(ms as f64 / 1000.0);
            if g != prev {
                edges += 1;
            }
            prev = g;
        }
        assert_eq!(edges, 4, "the menu cursor toggles 4 times per real second");
        // A garbage clock is phase 0, never a panic.
        assert_eq!(menu_cursor_glyph(f64::NAN), 12);
        assert_eq!(menu_cursor_glyph(-3.0), 12);
    }

    #[test]
    fn console_cursor_flashes_at_con_cursorspeed_on_realtime() {
        // Con_DrawInput: 10 + ((int)(realtime*con_cursorspeed) & 1), speed 4.
        // (The port used to blink cell 11 at 2 Hz off the host clock.)
        assert_eq!(console_cursor_glyph(0.0), 10, "cell 10 (blank) first");
        assert_eq!(console_cursor_glyph(0.3), 11, "the block from 0.25 s");
        assert_eq!(console_cursor_glyph(0.6), 10);
        assert_eq!(console_cursor_glyph(0.8), 11);
        let mut edges = 0;
        let mut prev = console_cursor_glyph(0.0);
        for ms in 1..=1000 {
            let g = console_cursor_glyph(ms as f64 / 1000.0);
            if g != prev {
                edges += 1;
            }
            prev = g;
        }
        assert_eq!(edges, 4, "the console cursor toggles 4 times per real second");
    }

    #[test]
    fn draw_menu_options_cursor_follows_realtime_not_host_time() {
        // End to end through draw_menu: a conchars whose cell 13 is lit and
        // cell 12 is blank (like id's), cursor on the Options top row at
        // (200, 32). host_time is held where the OLD parity code would have
        // shown the arrow (frame 1 = 0.1 s); only realtime decides.
        let pal = ramp_palette();
        let mut data = vec![0u8; 128 * 128];
        for y in 0..8 {
            for x in 0..8 {
                data[y * 128 + 13 * 8 + x] = 3; // cell 13 = (13, 0)
            }
        }
        let conchars = crate::wad::Qpic { width: 128, height: 128, data };
        let mut m = Menu::new();
        m.open();
        m.cursor = 2;
        m.select(); // -> Options, cursor row 0
        let px = 32 * 320 + 200;
        let mut off = Image::new(320, 200, [0, 0, 0]);
        draw_menu(&mut off, &m, &MenuPics::default(), Some(&conchars), 0.1, 0.1, &pal);
        assert_eq!(off.rgb[px], [0, 0, 0], "realtime 0.1 s: cursor phase blank");
        let mut on = Image::new(320, 200, [0, 0, 0]);
        draw_menu(&mut on, &m, &MenuPics::default(), Some(&conchars), 0.0, 0.3, &pal);
        assert_eq!(on.rgb[px], pal[3], "realtime 0.3 s: the arrow shows");
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

    // -- options menu (MenuScreen::Options + adjust + draw) -----------------

    #[test]
    fn menu_options_enter_from_main_and_back() {
        let mut m = Menu::new();
        m.open();
        // Main > Options (cursor 2) switches to the Options screen, no host action.
        m.cursor = 2;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Options);
        assert_eq!(m.cursor(), 0, "entering Options resets the cursor to the top row");
        assert!(m.visible);
        // Up wraps from row 0 to the last row (12), down wraps 12 -> 0: the cursor
        // covers all OPTIONS_ITEMS (13) rows.
        m.move_cursor(-1);
        assert_eq!(m.cursor(), OPTIONS_ITEMS - 1, "up from row 0 wraps to the last row");
        m.move_cursor(1);
        assert_eq!(m.cursor(), 0, "down from the last row wraps to 0");
        // Walk down through every row once.
        for expect in 1..OPTIONS_ITEMS {
            m.move_cursor(1);
            assert_eq!(m.cursor(), expect);
        }
        m.move_cursor(1);
        assert_eq!(m.cursor(), 0, "past the last row wraps to 0");
        // Escape backs out of Options to Main (still visible).
        assert_eq!(m.cancel(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Main);
        assert!(m.visible);
    }

    #[test]
    fn menu_screen_size_row_steps_viewsize_by_10_clamped_30_to_120() {
        // M_AdjustSliders case 3: scr_viewsize += dir*10, clamped 30..=120 —
        // the Screen size row is viewsize, NOT the video mode (the old port
        // cycled render resolutions here; WinQuake keeps those in M_Video).
        let mut m = Menu::new();
        m.open();
        m.cursor = 2;
        m.select(); // -> Options
        m.cursor = ROW_SCREENSIZE;
        assert_eq!(m.screen(), MenuScreen::Options);
        assert_eq!(m.viewsize(), 100.0, "default.cfg: viewsize 100");
        let mode = m.resolution();
        m.adjust(1);
        assert_eq!(m.viewsize(), 110.0);
        m.adjust(1);
        assert_eq!(m.viewsize(), 120.0);
        m.adjust(1);
        assert_eq!(m.viewsize(), 120.0, "clamped at 120 (no wrap)");
        for expect in [110.0, 100.0, 90.0, 80.0, 70.0, 60.0, 50.0, 40.0, 30.0, 30.0] {
            m.adjust(-1);
            assert_eq!(m.viewsize(), expect);
        }
        assert_eq!(m.resolution(), mode, "Screen size never touches the video mode");
        // Enter falls through to M_AdjustSliders(1) (menu2 + menu3), no host action.
        m.take_sounds();
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.viewsize(), 40.0);
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2, MenuSound::Menu3]);
        // A zero delta is a no-op; adjust only acts on the Options screen.
        m.adjust(0);
        assert_eq!(m.viewsize(), 40.0);
        m.cancel(); // -> Main
        m.adjust(1);
        assert_eq!(m.viewsize(), 40.0, "adjust is a no-op off the Options screen");
        // Reset to defaults: default.cfg's `viewsize 100`.
        m.reset_defaults();
        assert_eq!(m.viewsize(), 100.0);
    }

    #[test]
    fn options_screen_size_slider_tracks_viewsize() {
        // M_Options_Draw: r = (scr_viewsize - 30) / (120 - 30); the knob (glyph
        // 131) sits at 220 + 72*r on the Screen-size row (y = 56).
        let pal = ramp_palette();
        let mut data = vec![0u8; 128 * 128];
        for y in 0..8 {
            for x in 0..8 {
                data[(8 * 8 + y) * 128 + 3 * 8 + x] = 3; // cell 131 = (3, 8)
            }
        }
        let conchars = crate::wad::Qpic { width: 128, height: 128, data };
        let mut m = Menu::new();
        m.open();
        m.cursor = 2;
        m.select(); // -> Options
        let knob_x = |m: &Menu| {
            let mut img = Image::new(320, 200, [0, 0, 0]);
            draw_menu(&mut img, m, &MenuPics::default(), Some(&conchars), 0.0, 0.0, &pal);
            (0..320).find(|&x| img.rgb[56 * 320 + x] == pal[3]).expect("knob drawn")
        };
        assert_eq!(knob_x(&m), 276, "viewsize 100: r = 70/90 -> 220 + 56");
        m.set_viewsize(30.0);
        assert_eq!(knob_x(&m), 220, "viewsize 30: the left end");
        m.set_viewsize(120.0);
        assert_eq!(knob_x(&m), 292, "viewsize 120: the right end");
    }

    #[test]
    fn sizeup_sizedown_and_the_viewsize_cvar_bound_like_scr_calcrefdef() {
        let mut m = Menu::new();
        m.size_up();
        assert_eq!(m.viewsize(), 110.0);
        m.size_up();
        m.size_up();
        assert_eq!(m.viewsize(), 120.0, "sizeup stops at 120");
        for _ in 0..20 {
            m.size_down();
        }
        assert_eq!(m.viewsize(), 30.0, "sizedown stops at 30");
        // The console can set any value in range (not just multiples of 10);
        // out-of-range and garbage clamp like SCR_CalcRefdef's bound.
        m.set_viewsize(55.0);
        assert_eq!(m.viewsize(), 55.0);
        m.size_up();
        assert_eq!(m.viewsize(), 65.0);
        m.set_viewsize(7.0);
        assert_eq!(m.viewsize(), 30.0);
        m.set_viewsize(1e9);
        assert_eq!(m.viewsize(), 120.0);
        m.set_viewsize(f32::NAN);
        assert_eq!(m.viewsize(), 30.0, "atof garbage = 0 -> the minimum");
        // default.cfg binds + and = to sizeup and - to sizedown, as ordinary
        // (rebindable) bindings that Customize controls doesn't list.
        let m = Menu::new();
        assert_eq!(m.action_for_key(b'+'), Some(BIND_SIZEUP));
        assert_eq!(m.action_for_key(b'='), Some(BIND_SIZEUP));
        assert_eq!(m.action_for_key(b'-'), Some(BIND_SIZEDOWN));
        // Customize controls (the BINDNAMES rows) never lists them.
        let listed = (0..NUM_BINDNAMES).flat_map(|c| m.find_keys_for_command(c));
        for k in listed.flatten() {
            assert!(![b'+', b'=', b'-'].contains(&k), "key {k} is not a Keys-screen row");
        }
    }

    #[test]
    fn menu_adjust_clamps_mouse_and_volume() {
        let mut m = Menu::new();
        m.open();
        m.cursor = 2;
        m.select(); // -> Options

        // Mouse Speed row: default sensitivity 3 -> 1.0x multiplier.
        m.cursor = ROW_MOUSESPEED;
        assert!((m.sensitivity() - SENS_DEFAULT).abs() < 1e-6);
        assert!((m.mouse_sensitivity() - 1.0).abs() < 1e-6, "default mouse is 1.0x");
        // Decreasing clamps at SENS_MIN (1), never below.
        for _ in 0..40 {
            m.adjust(-1);
        }
        assert!((m.sensitivity() - SENS_MIN).abs() < 1e-6);
        // Increasing clamps at SENS_MAX (11).
        for _ in 0..60 {
            m.adjust(1);
        }
        assert!((m.sensitivity() - SENS_MAX).abs() < 1e-6);
        assert!(m.mouse_sensitivity() > 1.0, "max sensitivity is more than default");

        // Sound Volume row: default 0.7.
        m.cursor = ROW_SNDVOLUME;
        assert!((m.volume() - VOLUME_DEFAULT).abs() < 1e-6, "default volume is 0.7");
        for _ in 0..40 {
            m.adjust(-1);
        }
        assert!((m.volume() - VOLUME_MIN).abs() < 1e-6, "min volume is silent");
        for _ in 0..40 {
            m.adjust(1);
        }
        assert!((m.volume() - VOLUME_MAX).abs() < 1e-6, "max volume is full gain");
    }

    #[test]
    fn draw_menu_options_screen_draws_without_panic() {
        let pal = ramp_palette();
        // A conchars atlas where every glyph texel is the lit index 3 (except the
        // byte-0 cell), so any drawn label/value paints index-3 pixels.
        let mut data = vec![3u8; 128 * 128];
        for y in 0..8 {
            for x in 0..8 {
                data[y * 128 + x] = 0;
            }
        }
        let conchars = crate::wad::Qpic { width: 128, height: 128, data };

        let mut m = Menu::new();
        m.open();
        m.cursor = 2;
        m.select(); // -> Options
        assert_eq!(m.screen(), MenuScreen::Options);

        // Only the title pic is present (the cursor is a conchars glyph now, drawn
        // at x=200, not a menudot).
        let pics = MenuPics {
            p_option: Some(solid_pic(120, 24, 5)),
            ..Default::default()
        };

        let bg = [9u8, 9, 9];
        let mut img = Image::new(320, 200, bg);
        let before = img.rgb.clone();
        draw_menu(&mut img, &m, &pics, Some(&conchars), 0.0, 0.0, &pal);
        // The Options screen must change pixels over the known background.
        assert_ne!(img.rgb, before, "the Options screen must draw something");
        // The title plaque (index 5) paints centered near the top: at virtual
        // (100, 4) with a 120-wide pic centered ((320-120)/2 = 100).
        let title_idx = 4 * img.w + 100;
        assert_eq!(img.rgb[title_idx], pal[5], "the OPTIONS title must paint at the top");
        // The flashing cursor (conchars glyph 12/13, all-lit in this atlas -> index
        // 3) sits at virtual (200, 32) on the top row.
        let cursor_idx = 32 * img.w + 200;
        assert_eq!(img.rgb[cursor_idx], pal[3], "the cursor glyph must paint at x=200 on the top row");
        // A label glyph (index 3) paints on the first row at the label column x=16
        // (the "Customize controls" row is right-justified, so its first non-space
        // glyph lands a few cells in; check at x=48 which is inside the text).
        let label_idx = 32 * img.w + 48;
        assert_eq!(img.rgb[label_idx], pal[3], "the first Options label must paint");
        // A slider on the Screen-size row (row 3, y=32+3*8=56): glyphs at x>=212
        // (the left cap is at 220-8=212). Check the left cap pixel.
        let slider_idx = 56 * img.w + 212;
        assert_eq!(img.rgb[slider_idx], pal[3], "the Screen-size slider must paint at y=56");

        // The SAME screen also renders correctly at a LARGER framebuffer (640x400,
        // scale 2): it must not panic and must draw the title + cursor scaled.
        let mut big = Image::new(640, 400, bg);
        let big_before = big.rgb.clone();
        draw_menu(&mut big, &m, &pics, Some(&conchars), 0.0, 0.0, &pal);
        assert_ne!(big.rgb, big_before, "the Options screen draws at 640x400 too");
        // At scale 2 the cursor's virtual (200,32) maps to pixel (400,64).
        let big_cursor_idx = 64 * big.w + 400;
        assert_eq!(big.rgb[big_cursor_idx], pal[3], "cursor scales to (400,64) at 640x400");

        // Missing conchars leaves labels/widgets/cursor blank but still draws the
        // title; no panic.
        let mut img2 = Image::new(320, 200, bg);
        draw_menu(&mut img2, &m, &pics, None, 0.0, 0.0, &pal);
        assert_eq!(img2.rgb[title_idx], pal[5], "title still draws without conchars");
        assert_eq!(img2.rgb[cursor_idx], bg, "cursor needs conchars (blank without it)");
    }

    // -- new Options widgets / Help / Quit pure helpers ----------------------

    #[test]
    fn slider_knob_offset_maps_fraction_to_position() {
        // The knob travels from 0 (fraction 0) to (SLIDER_RANGE-1)*8 (fraction 1).
        let span = (SLIDER_RANGE - 1) as f32 * 8.0; // 9*8 = 72.
        assert_eq!(slider_knob_offset(0.0), 0.0, "fraction 0 -> left of the trough");
        assert_eq!(slider_knob_offset(1.0), span, "fraction 1 -> right end of the segments");
        assert!((slider_knob_offset(0.5) - span * 0.5).abs() < 1e-6, "midpoint is halfway");
        // Out-of-range fractions clamp to [0,1]; non-finite is treated as 0.
        assert_eq!(slider_knob_offset(-3.0), 0.0, "below 0 clamps to the left");
        assert_eq!(slider_knob_offset(7.0), span, "above 1 clamps to the right");
        assert_eq!(slider_knob_offset(f32::NAN), 0.0, "NaN is treated as 0");
        assert_eq!(slider_knob_offset(f32::INFINITY), 0.0, "infinity is treated as 0");
        // Monotone increasing across the range.
        let mut prev = -1.0;
        for i in 0..=10 {
            let v = slider_knob_offset(i as f32 / 10.0);
            assert!(v >= prev, "knob position must be non-decreasing in the fraction");
            prev = v;
        }
    }

    #[test]
    fn checkbox_text_is_on_or_off() {
        assert_eq!(checkbox_text(true), "on");
        assert_eq!(checkbox_text(false), "off");
    }

    #[test]
    fn help_page_wrap_clamps_into_range() {
        // In-range stays put.
        for p in 0..NUM_HELP_PAGES {
            assert_eq!(help_page_wrap(p as i32), p);
        }
        // Past the last page wraps to 0; below 0 wraps to the last page.
        assert_eq!(help_page_wrap(NUM_HELP_PAGES as i32), 0, "one past the end wraps to 0");
        assert_eq!(help_page_wrap(-1), NUM_HELP_PAGES - 1, "below 0 wraps to the last page");
        // Large magnitudes wrap modulo the page count, never panic / out-of-range.
        assert_eq!(help_page_wrap(13), (13 % NUM_HELP_PAGES as i32) as usize);
        assert!(help_page_wrap(i32::MAX) < NUM_HELP_PAGES);
        assert!(help_page_wrap(i32::MIN) < NUM_HELP_PAGES);
    }

    #[test]
    fn options_cursor_wraps_over_all_thirteen_rows() {
        // The cursor must visit every one of the 13 OPTIONS_ITEMS rows and wrap.
        let mut m = Menu::new();
        m.open();
        m.cursor = 2;
        m.select(); // -> Options
        assert_eq!(MenuScreen::Options.item_count(), 13);
        assert_eq!(m.cursor(), 0);
        let mut seen = [false; OPTIONS_ITEMS];
        for _ in 0..OPTIONS_ITEMS {
            seen[m.cursor()] = true;
            m.move_cursor(1);
        }
        assert!(seen.iter().all(|&v| v), "every Options row must be reachable");
        assert_eq!(m.cursor(), 0, "a full lap returns to row 0");
        // A big positive delta wraps modulo 13.
        m.cursor = 0;
        m.move_cursor(40); // 40 % 13 = 1
        assert_eq!(m.cursor(), 1);
    }

    #[test]
    fn options_sliders_and_checkboxes_adjust_per_row() {
        let mut m = Menu::new();
        m.open();
        m.cursor = 2;
        m.select(); // -> Options

        // Brightness (gamma) row: matching the C `v_gamma -= dir*0.05`, RIGHT
        // brightens (gamma DOWN toward 0.5), LEFT dims (gamma UP toward 1.0).
        m.cursor = ROW_BRIGHTNESS;
        assert!((m.gamma() - GAMMA_DEFAULT).abs() < 1e-6);
        for _ in 0..40 {
            m.adjust(1);
        }
        assert!((m.gamma() - GAMMA_MIN).abs() < 1e-6, "right clamps gamma at 0.5 (brightest)");
        for _ in 0..40 {
            m.adjust(-1);
        }
        assert!((m.gamma() - GAMMA_MAX).abs() < 1e-6, "left clamps gamma at 1.0 (dimmest)");

        // CD Music Volume row: 0..=1.
        m.cursor = ROW_CDVOLUME;
        for _ in 0..40 {
            m.adjust(-1);
        }
        assert!((m.bgm_volume() - BGM_MIN).abs() < 1e-6);
        for _ in 0..40 {
            m.adjust(1);
        }
        assert!((m.bgm_volume() - BGM_MAX).abs() < 1e-6);

        // Checkboxes toggle regardless of direction (matches the C). Always Run
        // starts ON (this port's default); the rest start off.
        for (row, getter, initial) in [
            (ROW_ALWAYSRUN, Menu::always_run as fn(&Menu) -> bool, true),
            (ROW_INVERTMOUSE, Menu::invert_mouse, false),
            (ROW_LOOKSPRING, Menu::lookspring, false),
            (ROW_LOOKSTRAFE, Menu::lookstrafe, false),
        ] {
            m.cursor = row;
            assert_eq!(getter(&m), initial, "checkbox row {row} starts at its default");
            m.adjust(1);
            assert_eq!(getter(&m), !initial, "right toggles it");
            m.adjust(-1);
            assert_eq!(getter(&m), initial, "left toggles it back");
        }
    }

    #[test]
    fn options_enter_actions_console_defaults_and_stubs() {
        let mut m = Menu::new();
        m.open();
        m.cursor = 2;
        m.select(); // -> Options

        // Go to console: closes the menu, returns OpenConsole.
        m.cursor = ROW_CONSOLE;
        assert_eq!(m.select(), MenuAction::OpenConsole);
        assert!(!m.visible, "Go to console closes the menu");

        // Reset to defaults: from non-default values, Enter restores them.
        m.open();
        m.cursor = 2;
        m.select(); // -> Options
        m.cursor = ROW_MOUSESPEED;
        m.adjust(1);
        m.adjust(1);
        m.cursor = ROW_ALWAYSRUN;
        m.adjust(1); // toggles OFF (Always Run defaults on in this port)
        assert!(m.sensitivity() != SENS_DEFAULT && !m.always_run());
        m.cursor = ROW_DEFAULTS;
        assert_eq!(m.select(), MenuAction::ResetDefaults);
        assert!((m.sensitivity() - SENS_DEFAULT).abs() < 1e-6, "defaults restored");
        assert!(m.always_run(), "Always Run resets to ON (the port default)");

        // Customize controls opens the Keys screen (M_Menu_Keys_f); Escape
        // returns to Options (M_Keys_Key K_ESCAPE -> M_Menu_Options_f).
        m.cursor = ROW_CONTROLS;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Keys, "Customize controls enters Keys");
        assert_eq!(m.cancel(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Options, "Esc on Keys returns to Options");

        // Video Options opens the mode list (M_Menu_Video_f) with the cursor on
        // the current preset; Escape returns to Options (VID_MenuKey K_ESCAPE).
        m.cursor = ROW_VIDEO;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Video, "Video Options enters the mode list");
        assert_eq!(m.cursor(), m.res_preset, "video cursor starts on the current mode");
        assert_eq!(m.cancel(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Options, "Esc on Video returns to Options");

        // Enter on an analog row nudges it right (the C falls through to
        // M_AdjustSliders(1)).
        m.cursor = ROW_SNDVOLUME;
        let before = m.volume();
        m.select();
        assert!(m.volume() > before, "Enter on Sound Volume nudges it up");
    }

    #[test]
    fn help_screen_pages_and_backs_out() {
        let mut m = Menu::new();
        m.open();
        // Main > Help.
        m.cursor = 3;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Help);
        assert_eq!(m.help_page(), 0);
        // Right/up advance the page (page(+1) = next), wrapping at NUM_HELP_PAGES.
        for expect in 1..NUM_HELP_PAGES {
            m.page(1);
            assert_eq!(m.help_page(), expect);
        }
        m.page(1);
        assert_eq!(m.help_page(), 0, "past the last page wraps to 0");
        // Left/down go back (page(-1) = prev), wrapping below 0.
        m.page(-1);
        assert_eq!(m.help_page(), NUM_HELP_PAGES - 1, "below 0 wraps to the last page");
        // Up/down on the Help screen also page (M_Help_Key): the host passes
        // up = move_cursor(-1) / down = move_cursor(+1), and per the C UP advances
        // (m_help_page++) while DOWN goes back (m_help_page--).
        m.help_page = 0;
        m.move_cursor(1); // down -> previous page (wraps below 0)
        assert_eq!(
            m.help_page(),
            NUM_HELP_PAGES - 1,
            "down pages backward on Help (wraps to the last page)"
        );
        m.move_cursor(-1); // up -> next page (wraps back to 0)
        assert_eq!(m.help_page(), 0, "up pages forward on Help");
        // page() is a no-op off the Help screen.
        m.cancel(); // -> Main
        assert_eq!(m.screen(), MenuScreen::Main);
        m.page(1);
        assert_eq!(m.help_page(), 0, "page() does nothing off the Help screen");
    }

    #[test]
    fn gamma_table_identity_at_one_and_curve_below() {
        // BuildGammaTable's g == 1.0 special case is a literal identity — the
        // host skips the LUT entirely there, so default gamma is byte-exact.
        let id = build_gamma_table(1.0);
        for (i, &v) in id.iter().enumerate() {
            assert_eq!(v as usize, i, "gamma 1.0 must be the identity at {i}");
        }
        // Below 1.0 the curve BRIGHTENS (x^g > x for x in (0,1), g < 1) and is
        // monotonic; the top end stays pinned by the clamp.
        let g = build_gamma_table(0.5);
        assert_eq!(g[255], 255);
        for i in 1..255usize {
            assert!(g[i] >= id[i], "gamma 0.5 must brighten every level ({i})");
            assert!(g[i] >= g[i - 1], "gamma table must be monotonic ({i})");
        }
        // The C's exact formula spot-check: i=64, g=0.5 ->
        // 255*sqrt(64.5/255.5)+0.5 = 128.6... -> truncates to 128.
        assert_eq!(g[64], 128);
        // ...and the bottom level: 255*sqrt(0.5/255.5)+0.5 = 11.78 -> 11.
        assert_eq!(g[0], 11);
    }

    #[test]
    fn menu_sounds_follow_the_c_triggers() {
        let mut m = Menu::new();
        // Opening latches m_entersound -> menu2.
        m.open();
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2]);
        // Cursor moves play menu1 per press (M_Main_Key K_UP/DOWNARROW).
        m.move_cursor(1);
        m.move_cursor(-1);
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu1, MenuSound::Menu1]);
        // Entering a submenu plays menu2 (M_Main_Key K_ENTER latches it).
        m.cursor = 2;
        m.select(); // -> Options
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2]);
        // Left/right adjust plays menu3 (M_AdjustSliders' unconditional
        // S_LocalSound) — even when the cursor sits on an action row.
        m.cursor = ROW_SNDVOLUME;
        m.adjust(-1);
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu3]);
        m.cursor = ROW_CONTROLS;
        m.adjust(1);
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu3], "menu3 plays on action rows too");
        // Enter on a slider row: m_entersound (menu2) AND M_AdjustSliders' menu3.
        m.cursor = ROW_BRIGHTNESS;
        m.select();
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2, MenuSound::Menu3]);
        // Escape back to Main: M_Menu_Main_f latches menu2.
        m.cancel();
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2]);
        // Going to the console CLOSES the menu — the C's latched entersound
        // never fires (M_Draw stops running): silent.
        m.cursor = 2;
        m.select(); // -> Options (menu2)
        m.take_sounds();
        m.cursor = ROW_CONSOLE;
        assert_eq!(m.select(), MenuAction::OpenConsole);
        assert_eq!(m.take_sounds(), vec![], "closing into the console is silent");
        // The sample names match S_LocalSound's literals.
        assert_eq!(MenuSound::Menu1.sample(), "misc/menu1.wav");
        assert_eq!(MenuSound::Menu2.sample(), "misc/menu2.wav");
        assert_eq!(MenuSound::Menu3.sample(), "misc/menu3.wav");
        // The queue is bounded even when the host never drains.
        m.open();
        for _ in 0..100 {
            m.move_cursor(1);
        }
        assert!(m.take_sounds().len() <= MENU_SOUND_CAP);
    }

    #[test]
    fn load_save_screens_slots_gate_and_actions() {
        let mut m = Menu::new();
        m.open();
        m.select(); // -> SinglePlayer

        // Item 2 = Save: REFUSED while no game is running (M_Menu_Save_f's
        // `if (!sv.active) return`). The entersound was latched before the
        // early return, so menu2 still plays.
        m.cursor = 2;
        m.take_sounds();
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::SinglePlayer, "Save refuses without a game");
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2]);

        // Item 1 = Load (M_Menu_Load_f) opens with all slots unused.
        m.cursor = 1;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Load);
        for i in 0..MAX_SAVEGAMES {
            assert!(!m.slot_loadable(i), "slot {i} must start unused");
            assert_eq!(m.save_comment(i), "");
        }
        // 12 slots; the cursor wraps over them, and left/right pair with
        // up/down (M_Load_Key K_LEFTARROW == K_UPARROW).
        for expect in [1, 2, 3] {
            m.move_cursor(1);
            assert_eq!(m.cursor(), expect);
        }
        m.adjust(-1);
        assert_eq!(m.cursor(), 2);
        m.cursor = MAX_SAVEGAMES - 1;
        m.move_cursor(1);
        assert_eq!(m.cursor(), 0, "load cursor wraps over MAX_SAVEGAMES");
        // Enter on an unused slot: menu2 plays but nothing happens — the C's
        // `if (!loadable[load_cursor]) return`.
        m.take_sounds();
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Load, "unused slot doesn't leave the screen");
        assert!(m.visible);
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2], "Load Enter still plays menu2");

        // Host fills slot 2 (a savegame engine ran M_ScanSaves): it becomes
        // loadable and Enter emits LoadSlot(2) + closes the menu.
        let mut comments: [String; MAX_SAVEGAMES] = Default::default();
        comments[2] = "e1m1: Slipgate Complex".to_string();
        m.set_save_comments(comments);
        assert!(m.slot_loadable(2));
        assert!(!m.slot_loadable(3));
        m.cursor = 2;
        m.take_sounds();
        assert_eq!(m.select(), MenuAction::LoadSlot(2));
        assert!(!m.visible, "a real load closes the menu (m_state = m_none)");
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2]);

        // Escape on Load returns to SinglePlayer (M_Load_Key K_ESCAPE).
        m.open();
        m.select(); // -> SinglePlayer (cursor 0)
        m.cursor = 1;
        m.select(); // -> Load
        assert_eq!(m.cancel(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::SinglePlayer);

        // With a game running, Save opens; Enter emits SaveSlot for the
        // highlighted slot, closes the menu, and is SILENT (M_Save_Key K_ENTER
        // plays nothing).
        m.set_game_active(true);
        m.cursor = 2;
        assert_eq!(m.select(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Save);
        m.cursor = 5;
        m.take_sounds();
        assert_eq!(m.select(), MenuAction::SaveSlot(5));
        assert!(!m.visible, "Save Enter closes the menu like the C");
        assert_eq!(m.take_sounds(), vec![], "Save Enter is silent in the C");

        // save_comment is total (out-of-range = empty).
        assert_eq!(m.save_comment(MAX_SAVEGAMES + 3), "");
    }

    #[test]
    fn video_screen_lists_and_applies_presets() {
        let mut m = Menu::new();
        m.sync_resolution(RESOLUTION_PRESETS[2].0, RESOLUTION_PRESETS[2].1);
        m.open();
        m.cursor = 2;
        m.select(); // -> Options
        m.cursor = ROW_VIDEO;
        m.select(); // -> Video
        assert_eq!(m.screen(), MenuScreen::Video);
        assert_eq!(m.cursor(), 2, "cursor opens on the current mode");
        // Move to another mode and apply it: VID_MenuKey K_ENTER -> VID_SetMode.
        m.move_cursor(1);
        m.take_sounds();
        assert_eq!(m.select(), MenuAction::ResolutionChanged);
        assert_eq!(m.resolution(), RESOLUTION_PRESETS[3]);
        assert_eq!(
            m.take_sounds(),
            vec![MenuSound::Menu1],
            "VID_MenuKey K_ENTER plays menu1 (not menu2)"
        );
        assert_eq!(m.screen(), MenuScreen::Video, "the mode list stays up after applying");
        // The cursor wraps over the preset list; left/right also step it.
        m.cursor = RESOLUTION_PRESETS.len() - 1;
        m.move_cursor(1);
        assert_eq!(m.cursor(), 0);
        let mode = m.resolution();
        m.adjust(1);
        assert_eq!(m.cursor(), 1, "video left/right move the line");
        assert_eq!(m.resolution(), mode, "...but only Enter sets the mode");
    }

    #[test]
    fn keys_screen_lists_rebinds_and_unbinds() {
        let mut m = Menu::new();
        // The defaults include id's default.cfg keys and the port's WASD layout.
        assert_eq!(m.action_for_key(b'w'), Some(BIND_FORWARD));
        assert_eq!(m.action_for_key(K_UPARROW), Some(BIND_FORWARD));
        assert_eq!(m.action_for_key(K_MOUSE1), Some(BIND_ATTACK));
        assert_eq!(m.action_for_key(K_CTRL), Some(BIND_ATTACK));
        assert_eq!(m.action_for_key(K_SPACE), Some(BIND_JUMP));
        assert_eq!(m.action_for_key(b'/'), Some(BIND_CHANGEWEAPON));
        assert_eq!(m.action_for_key(b'c'), Some(BIND_MOVEDOWN));
        assert_eq!(m.action_for_key(K_SHIFT), Some(BIND_SPEED));
        // find_keys_for_command returns up to two keys in keynum order
        // (M_FindKeysForCommand scans 0..256 ascending: 'w' = 119 < 128).
        assert_eq!(m.find_keys_for_command(BIND_FORWARD), [Some(b'w'), Some(K_UPARROW)]);

        // Navigate Main > Options > Customize controls.
        m.open();
        m.cursor = 2;
        m.select();
        m.cursor = ROW_CONTROLS;
        m.select();
        assert_eq!(m.screen(), MenuScreen::Keys);
        assert!(!m.bind_grabbing());

        // Enter on "+attack" (row 0, already two keys: CTRL + MOUSE1): the C
        // unbinds first, then grabs.
        m.cursor = BIND_ATTACK;
        m.take_sounds();
        assert_eq!(m.select(), MenuAction::None);
        assert!(m.bind_grabbing(), "Enter starts the bind grab");
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2]);
        assert_eq!(
            m.find_keys_for_command(BIND_ATTACK),
            [None, None],
            "two-key rows unbind before grabbing"
        );
        // Deliver the grabbed key: 'x' binds to +attack, menu1 plays.
        m.bind_key(b'x');
        assert!(!m.bind_grabbing());
        assert_eq!(m.action_for_key(b'x'), Some(BIND_ATTACK));
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu1]);

        // Escape during a grab cancels without binding.
        m.select();
        assert!(m.bind_grabbing());
        m.bind_key(K_ESCAPE);
        assert!(!m.bind_grabbing());
        assert_eq!(m.action_for_key(K_ESCAPE), None, "Escape never binds");
        // The console key is refused too (the C's `k != '`'` check).
        m.select();
        m.bind_key(b'`');
        assert_eq!(m.action_for_key(b'`'), None, "backtick never binds");

        // cancel() during a grab also just ends the grab (screen stays).
        m.select();
        assert!(m.bind_grabbing());
        assert_eq!(m.cancel(), MenuAction::None);
        assert!(!m.bind_grabbing());
        assert_eq!(m.screen(), MenuScreen::Keys, "Esc in grab stays on Keys");

        // Backspace unbinds the highlighted command (menu2).
        m.cursor = BIND_FORWARD;
        m.take_sounds();
        m.keys_backspace();
        assert_eq!(m.find_keys_for_command(BIND_FORWARD), [None, None]);
        assert_eq!(m.action_for_key(b'w'), None);
        assert_eq!(m.take_sounds(), vec![MenuSound::Menu2]);
        // ...and left/right move the keys cursor like up/down (M_Keys_Key).
        m.adjust(1);
        assert_eq!(m.cursor(), BIND_FORWARD + 1);

        // Reset to defaults re-execs default.cfg: the bindings come back.
        m.reset_defaults();
        assert_eq!(m.action_for_key(b'w'), Some(BIND_FORWARD));
        assert_eq!(m.action_for_key(b'x'), None, "custom binds reset too");
    }

    /// The host's re-boot sites (boot / boot_demo / boot_attract / New Game /
    /// `map`) reset the menu with [`Menu::reset_nav`]: navigation goes back to
    /// boot state but EVERY user choice survives — WinQuake's `map start`
    /// (M_SinglePlayer "New Game") never resets cvars or `keybindings[]` (they
    /// are host state, persisted by Host_WriteConfiguration). This locks the
    /// "rebind keys, set Always Run, then New Game" flow as a contract.
    #[test]
    fn reset_nav_keeps_user_choices_and_resets_navigation() {
        let mut m = Menu::new();
        m.open();
        // Change every class of user choice through the real menu paths.
        m.cursor = 2;
        m.select(); // Main > Options
        m.cursor = ROW_SCREENSIZE;
        m.adjust(-1); // viewsize 100 -> 90
        m.cursor = ROW_BRIGHTNESS;
        m.adjust(1); // v_gamma 1.0 -> 0.95 (RIGHT brightens: -= 0.05)
        m.cursor = ROW_MOUSESPEED;
        m.adjust(1); // sensitivity 3 -> 3.5
        m.cursor = ROW_SNDVOLUME;
        m.adjust(-1); // volume 0.7 -> 0.6
        m.cursor = ROW_CDVOLUME;
        m.adjust(-1); // bgmvolume 1.0 -> 0.9
        for row in [ROW_ALWAYSRUN, ROW_INVERTMOUSE, ROW_LOOKSPRING, ROW_LOOKSTRAFE] {
            m.cursor = row;
            m.adjust(1); // toggles flip regardless of direction (Always Run: on -> OFF)
        }
        // Rebind through the real grab path: Options > Customize controls,
        // Enter on "jump / swim up" (one key bound — no unbind-first), 'j'.
        m.cursor = ROW_CONTROLS;
        m.select(); // -> Keys
        m.cursor = BIND_JUMP;
        m.select(); // starts the grab
        m.bind_key(b'j');
        assert_eq!(m.action_for_key(b'j'), Some(BIND_JUMP));
        // Host-mirrored externals: slot comments + the Save gate.
        let mut comments: [String; MAX_SAVEGAMES] = Default::default();
        comments[3] = "e1m1 quick".to_string();
        m.set_save_comments(comments);
        m.set_game_active(true);

        // The re-boot reset.
        m.reset_nav();

        // Navigation is back at boot state...
        assert!(!m.visible, "reset_nav leaves the menu closed");
        assert_eq!(m.screen(), MenuScreen::Main);
        assert_eq!(m.cursor(), 0);
        assert_eq!(m.help_page(), 0);
        assert!(!m.bind_grabbing(), "a pending bind grab is cancelled");
        assert!(m.take_sounds().is_empty(), "queued menu sounds are dropped");
        // ...but EVERY user choice survives.
        assert_eq!(m.viewsize(), 90.0, "Screen size (viewsize) survives");
        assert!((m.gamma() - 0.95).abs() < 1e-6, "Brightness survives");
        assert!((m.sensitivity() - 3.5).abs() < 1e-6, "Mouse speed survives");
        assert!((m.volume() - 0.6).abs() < 1e-6, "Sound volume survives");
        assert!((m.bgm_volume() - 0.9).abs() < 1e-6, "CD volume survives");
        assert!(!m.always_run(), "Always Run (toggled off its on-default) survives");
        assert!(m.invert_mouse(), "Invert Mouse survives");
        assert!(m.lookspring(), "Lookspring survives");
        assert!(m.lookstrafe(), "Lookstrafe survives");
        assert_eq!(m.action_for_key(b'j'), Some(BIND_JUMP), "rebinds survive");
        assert_eq!(m.action_for_key(K_SPACE), Some(BIND_JUMP), "seeded binds survive");
        assert_eq!(m.action_for_key(b'w'), Some(BIND_FORWARD), "seeded binds survive");
        assert_eq!(m.save_comment(3), "e1m1 quick", "host-set slot comments survive");
        assert!(m.slot_loadable(3));
        assert!(m.game_active, "the Save gate is host state, not navigation");

        // And the menu still opens normally afterwards.
        m.open();
        assert!(m.visible);
        assert_eq!(m.screen(), MenuScreen::Main);
        assert_eq!(m.cursor(), 0);
    }

    #[test]
    fn keynum_names_match_key_keynum_to_string() {
        assert_eq!(keynum_to_string(b'a'), "a");
        assert_eq!(keynum_to_string(b'/'), "/");
        assert_eq!(keynum_to_string(K_SPACE), "SPACE");
        assert_eq!(keynum_to_string(K_UPARROW), "UPARROW");
        assert_eq!(keynum_to_string(K_MOUSE1), "MOUSE1");
        assert_eq!(keynum_to_string(K_F1), "F1");
        assert_eq!(keynum_to_string(K_F12), "F12");
        assert_eq!(keynum_to_string(K_DEL), "DEL");
        assert_eq!(keynum_to_string(0), "UNKNOWN");
    }

    #[test]
    fn draw_new_menu_screens_without_pics_dont_panic() {
        // Every new screen draws with NO pics and no conchars (worst case), and
        // with conchars only (the text paths) — nothing may panic, and the text
        // screens must put ink on the frame.
        let pal = ramp_palette();
        let pics = MenuPics::default();
        let conchars = test_conchars();
        let mut m = Menu::new();
        m.open();
        for (screen, cursor) in [
            (MenuScreen::Multiplayer, 0),
            (MenuScreen::Load, 3),
            (MenuScreen::Save, 11),
            (MenuScreen::Keys, 5),
            (MenuScreen::Video, 2),
        ] {
            m.screen = screen;
            m.cursor = cursor;
            let mut img = Image::new(320, 200, [9, 9, 9]);
            draw_menu(&mut img, &m, &pics, None, 0.4, 0.0, &pal); // no pics, no font
            let mut img2 = Image::new(320, 200, [9, 9, 9]);
            draw_menu(&mut img2, &m, &pics, Some(&conchars), 0.4, 0.0, &pal);
            let inked = img2.rgb.iter().any(|&p| p != [9, 9, 9]);
            assert!(inked, "{screen:?} must draw its text rows with conchars present");
        }
        // A host-set slot comment replaces the UNUSED text without panicking,
        // and the bind-grab prompt variant draws too.
        let mut comments: [String; MAX_SAVEGAMES] = Default::default();
        comments[0] = "a comment longer than the unused-slot text fits fine".into();
        m.set_save_comments(comments);
        m.screen = MenuScreen::Load;
        let mut img = Image::new(320, 200, [9, 9, 9]);
        draw_menu(&mut img, &m, &pics, Some(&conchars), 0.4, 0.0, &pal);
        m.screen = MenuScreen::Keys;
        m.bind_grab = true;
        let mut img = Image::new(320, 200, [9, 9, 9]);
        draw_menu(&mut img, &m, &pics, Some(&conchars), 0.4, 0.0, &pal);
    }

    #[test]
    fn quit_confirm_yes_no_flow() {
        let mut m = Menu::new();
        m.open();
        // Raise from Main via select.
        m.cursor = 4;
        m.select();
        assert_eq!(m.screen(), MenuScreen::Quit);
        // No (escape) restores Main.
        assert_eq!(m.quit_no(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Main);
        assert!(m.visible);
        // Raise again, Yes closes the menu.
        m.cursor = 4;
        m.select();
        assert_eq!(m.quit_yes(), MenuAction::Closed);
        assert!(!m.visible);

        // The prompt remembers a NON-Main origin (open_quit from Options -> No
        // restores Options).
        m.open();
        m.cursor = 2;
        m.select(); // -> Options
        m.open_quit();
        assert_eq!(m.screen(), MenuScreen::Quit);
        assert_eq!(m.cancel(), MenuAction::Back);
        assert_eq!(m.screen(), MenuScreen::Options, "No restores the prompt's origin screen");

        // quit_yes / quit_no are no-ops off the Quit screen.
        assert_eq!(m.quit_yes(), MenuAction::None);
        assert_eq!(m.quit_no(), MenuAction::None);
        assert_eq!(m.screen(), MenuScreen::Options, "no-op leaves the screen unchanged");
    }

    #[test]
    fn draw_help_and_quit_screens_without_panic() {
        let pal = ramp_palette();
        let mut data = vec![3u8; 128 * 128];
        for y in 0..8 {
            for x in 0..8 {
                data[y * 128 + x] = 0;
            }
        }
        let conchars = crate::wad::Qpic { width: 128, height: 128, data };
        let bg = [9u8, 9, 9];

        // Help: a present page pic (index 6) at (0,0) must paint the top-left.
        let mut help: [Option<crate::wad::Qpic>; NUM_HELP_PAGES] = Default::default();
        help[2] = Some(solid_pic(320, 200, 6));
        let pics = MenuPics { help, ..Default::default() };
        let mut m = Menu::new();
        m.open();
        m.cursor = 3;
        m.select(); // -> Help, page 0
        m.help_page = 2; // the page that has art
        let mut img = Image::new(320, 200, bg);
        draw_menu(&mut img, &m, &pics, Some(&conchars), 0.0, 0.0, &pal);
        assert_eq!(img.rgb[0], pal[6], "the help page pic must paint at (0,0)");
        // A missing page (page 0 here is None) draws nothing and never panics.
        m.help_page = 0;
        let mut img0 = Image::new(320, 200, bg);
        draw_menu(&mut img0, &m, &pics, Some(&conchars), 0.0, 0.0, &pal);
        assert_eq!(img0.rgb[0], bg, "a missing help page leaves the frame untouched");

        // Quit: the confirm box must paint (the dark box + the prompt text).
        m.open();
        m.cursor = 4;
        m.select(); // -> Quit
        assert_eq!(m.screen(), MenuScreen::Quit);
        let mut imgq = Image::new(320, 200, bg);
        let before = imgq.rgb.clone();
        draw_menu(&mut imgq, &m, &MenuPics::default(), Some(&conchars), 0.0, 0.0, &pal);
        assert_ne!(imgq.rgb, before, "the Quit prompt must draw something");
        // The dark box paints black inside its region (e.g. virtual (60,80)).
        let box_idx = 80 * imgq.w + 60;
        assert_eq!(imgq.rgb[box_idx], [0, 0, 0], "the Quit box is a dark fill");
        // Without conchars the box still paints (no panic).
        let mut imgq2 = Image::new(320, 200, bg);
        draw_menu(&mut imgq2, &m, &MenuPics::default(), None, 0.0, 0.0, &pal);
        assert_eq!(imgq2.rgb[box_idx], [0, 0, 0], "the Quit box paints without conchars");
    }

    // -----------------------------------------------------------------------
    // Near-plane polygon clipping (clip_poly_near)
    // -----------------------------------------------------------------------

    fn vv(vx: f32, vy: f32, vz: f32, s: f32, t: f32) -> VView {
        VView { vx, vy, vz, s, t }
    }

    /// (a) A polygon entirely in front of the near plane (every `vz > NEAR`) must
    /// come back UNCHANGED: same vertices, same order, byte-identical. This is the
    /// common case and any regression here would corrupt every visible wall.
    #[test]
    fn clip_poly_near_keeps_front_polygon_unchanged() {
        let poly = vec![
            vv(-2.0, -1.0, 5.0, 0.0, 0.0),
            vv(3.0, -1.0, 8.0, 64.0, 0.0),
            vv(3.0, 4.0, 8.0, 64.0, 64.0),
            vv(-2.0, 4.0, 5.0, 0.0, 64.0),
        ];
        let out = clip_poly_near(&poly);
        assert_eq!(out.len(), poly.len(), "front polygon must keep all vertices");
        for (o, p) in out.iter().zip(poly.iter()) {
            // Exact equality (no lerp should have run): bit-for-bit identical.
            assert_eq!(o.vx, p.vx);
            assert_eq!(o.vy, p.vy);
            assert_eq!(o.vz, p.vz);
            assert_eq!(o.s, p.s);
            assert_eq!(o.t, p.t);
        }
        // A vertex sitting exactly on the plane (vz == NEAR) counts as OUTSIDE
        // (inside is strictly vz > NEAR), so a polygon touching the plane is NOT
        // the trivial fast-path; but with all others in front it still clips to a
        // valid (>=3 vert) polygon.
        let touching = vec![
            vv(0.0, 0.0, NEAR_PLANE, 0.0, 0.0),
            vv(1.0, 0.0, 5.0, 10.0, 0.0),
            vv(1.0, 1.0, 5.0, 10.0, 10.0),
        ];
        let out = clip_poly_near(&touching);
        assert!(out.len() >= 3, "touching-plane triangle still clips to a polygon");
        for o in &out {
            assert!(o.vz >= NEAR_PLANE - 1e-4, "every output vertex is on/in front of NEAR");
        }
    }

    /// (b) A polygon entirely behind the near plane (every `vz <= NEAR`) yields
    /// fewer than 3 vertices, so the caller drops the face.
    #[test]
    fn clip_poly_near_drops_fully_behind_polygon() {
        let behind = vec![
            vv(-1.0, -1.0, -3.0, 0.0, 0.0),
            vv(1.0, -1.0, 0.0, 1.0, 0.0),
            vv(0.0, 1.0, NEAR_PLANE, 1.0, 1.0), // exactly on the plane = outside
        ];
        let out = clip_poly_near(&behind);
        assert!(out.len() < 3, "a fully-behind polygon must clip away (got {})", out.len());

        // An empty input is also handled (no panic, empty out).
        assert!(clip_poly_near(&[]).is_empty());
    }

    /// (c) A triangle straddling the near plane clips to a 4-vertex polygon: the
    /// two front vertices are kept verbatim and the two edges crossing the plane
    /// each contribute one new vertex with `vz == NEAR` and correctly-lerped
    /// `(vx, vy, s, t)`.
    #[test]
    fn clip_poly_near_straddling_triangle_lerps_correctly() {
        // Apex behind the plane, base in front. Numbers chosen so the crossings
        // land at simple parameters.
        //  A: behind   (vz = 0,  s=0,  t=0)
        //  B: in front (vz = 3,  s=30, t=0)
        //  C: in front (vz = 3,  s=30, t=30)
        let a = vv(0.0, 0.0, 0.0, 0.0, 0.0);
        let b = vv(6.0, 0.0, 3.0, 30.0, 0.0);
        let c = vv(6.0, 6.0, 3.0, 30.0, 30.0);
        let out = clip_poly_near(&[a, b, c]);
        assert_eq!(out.len(), 4, "an apex-behind triangle clips to a quad");

        // Sutherland–Hodgman walks edges A->B, B->C, C->A. With A outside and
        // B,C inside, the emitted ring is:
        //   edge A->B: A outside (skip A), crossing P (A->B), then keep B
        //   edge B->C: keep C
        //   edge C->A: crossing Q (C->A)
        // => [P, B, C, Q].
        //
        // Crossing on A->B at alpha = (NEAR - 0)/(3 - 0) = 1/3:
        //   vx = 0 + (6-0)*1/3 = 2, vz = NEAR = 1, s = 0 + 30/3 = 10, t = 0.
        // Crossing on C->A at alpha = (NEAR - 3)/(0 - 3) = 2/3:
        //   vx = 6 + (0-6)*2/3 = 2, vz = 1, s = 30 + (0-30)*2/3 = 10,
        //   t = 30 + (0-30)*2/3 = 10.
        let eps = 1e-5;
        // out[0] = P (A->B crossing)
        assert!((out[0].vz - NEAR_PLANE).abs() < eps, "P.vz must be NEAR");
        assert!((out[0].vx - 2.0).abs() < eps, "P.vx lerp");
        assert!((out[0].vy - 0.0).abs() < eps, "P.vy lerp");
        assert!((out[0].s - 10.0).abs() < eps, "P.s lerp");
        assert!((out[0].t - 0.0).abs() < eps, "P.t lerp");
        // out[1] = B (kept verbatim)
        assert_eq!(out[1].vx, b.vx);
        assert_eq!(out[1].vz, b.vz);
        assert_eq!(out[1].s, b.s);
        // out[2] = C (kept verbatim)
        assert_eq!(out[2].vx, c.vx);
        assert_eq!(out[2].vz, c.vz);
        assert_eq!(out[2].t, c.t);
        // out[3] = Q (C->A crossing)
        assert!((out[3].vz - NEAR_PLANE).abs() < eps, "Q.vz must be NEAR");
        assert!((out[3].vx - 2.0).abs() < eps, "Q.vx lerp");
        assert!((out[3].s - 10.0).abs() < eps, "Q.s lerp");
        assert!((out[3].t - 10.0).abs() < eps, "Q.t lerp");

        // Every output vertex is on or in front of the plane.
        for o in &out {
            assert!(o.vz >= NEAR_PLANE - eps, "clipped vertex behind NEAR: vz={}", o.vz);
        }
    }

    // -----------------------------------------------------------------------
    // Drop-down console
    // -----------------------------------------------------------------------

    /// A 128x128 conchars atlas where every glyph texel is the lit index 3,
    /// EXCEPT the byte-0 cell (top-left 8x8) which is index-0 transparent — so a
    /// drawn non-space character paints index-3 pixels and a space/byte-0 paints
    /// nothing, matching the menu tests' helper.
    fn lit_conchars() -> crate::wad::Qpic {
        let mut data = vec![3u8; 128 * 128];
        for y in 0..8 {
            for x in 0..8 {
                data[y * 128 + x] = 0;
            }
        }
        crate::wad::Qpic { width: 128, height: 128, data }
    }

    #[test]
    fn console_putchar_and_backspace_edit_the_input() {
        let mut c = Console::new();
        assert_eq!(c.input(), "");
        c.putchar('g');
        c.putchar('o');
        c.putchar('d');
        assert_eq!(c.input(), "god");
        c.backspace();
        assert_eq!(c.input(), "go");
        // The backtick/tilde toggle key and control chars never enter the buffer.
        c.putchar('`');
        c.putchar('~');
        c.putchar('\n');
        c.putchar('\t');
        assert_eq!(c.input(), "go", "toggle/control chars are not typed");
        // Backspacing an empty line is a harmless no-op.
        c.backspace();
        c.backspace();
        c.backspace();
        assert_eq!(c.input(), "");
        c.backspace();
        assert_eq!(c.input(), "");
        // The input length is capped.
        for _ in 0..(CONSOLE_INPUT_CAP + 50) {
            c.putchar('x');
        }
        assert_eq!(c.input().chars().count(), CONSOLE_INPUT_CAP, "input length is capped");
    }

    #[test]
    fn console_take_input_returns_and_clears_and_echoes() {
        let mut c = Console::new();
        for ch in "give h 100".chars() {
            c.putchar(ch);
        }
        let before = c.line_count();
        let got = c.take_input();
        assert_eq!(got.as_deref(), Some("give h 100"), "take_input returns the line");
        assert_eq!(c.input(), "", "take_input clears the input line");
        assert_eq!(c.line_count(), before + 1, "the submitted line is echoed to scrollback");
        // A blank line is a no-op: nothing returned, nothing echoed.
        let n = c.line_count();
        assert_eq!(c.take_input(), None, "blank input returns None");
        assert_eq!(c.line_count(), n, "blank input echoes nothing");
        for ch in "   ".chars() {
            c.putchar(ch);
        }
        assert_eq!(c.take_input(), None, "whitespace-only input returns None");
        assert_eq!(c.line_count(), n);
    }

    #[test]
    fn console_println_caps_the_scrollback() {
        let mut c = Console::new();
        // Push well past the cap; the history must never exceed it.
        for i in 0..(CONSOLE_SCROLLBACK_CAP * 2) {
            c.println(format!("line {i}"));
        }
        assert_eq!(
            c.line_count(),
            CONSOLE_SCROLLBACK_CAP,
            "scrollback is capped at CONSOLE_SCROLLBACK_CAP"
        );
        // A multi-line message counts as multiple lines (split on '\n').
        let mut c2 = Console::new();
        c2.println("a\nb\nc");
        assert_eq!(c2.line_count(), 3, "embedded newlines split into separate lines");
        // clear() empties the scrollback but not the input.
        for ch in "abc".chars() {
            c2.putchar(ch);
        }
        c2.clear();
        assert_eq!(c2.line_count(), 0, "clear empties the scrollback");
        assert_eq!(c2.input(), "abc", "clear leaves the input line untouched");
    }

    #[test]
    fn console_toggle_flips_open() {
        let mut c = Console::new();
        assert!(!c.open);
        c.toggle();
        assert!(c.open, "toggle opens");
        c.toggle();
        assert!(!c.open, "toggle closes");
    }

    #[test]
    fn draw_console_closed_is_a_noop_open_draws() {
        let pal = ramp_palette();
        let cc = lit_conchars();
        let conback = solid_pic(320, 200, 5); // opaque index-5 background

        let bg = [9u8, 9, 9];
        // Closed: draws nothing.
        let mut c = Console::new();
        let mut img = Image::new(320, 200, bg);
        let before = img.rgb.clone();
        draw_console(&mut img, &c, Some(&conback), Some(&cc), &pal, 0.0);
        assert_eq!(img.rgb, before, "a closed console draws nothing");

        // Open: the panel background paints index-5 across the TOP region.
        c.toggle();
        c.println("hello console");
        for ch in "god".chars() {
            c.putchar(ch);
        }
        draw_console(&mut img, &c, Some(&conback), Some(&cc), &pal, 0.0);
        assert_ne!(img.rgb, before, "an open console draws pixels");
        // A pixel in the top-left of the panel must be the conback colour (5).
        assert_eq!(img.rgb[2 * img.w + 2], pal[5], "the conback background paints at the top");
        // A pixel BELOW the panel (bottom of the frame) is untouched.
        let bottom = (img.h - 1) * img.w + 2;
        assert_eq!(img.rgb[bottom], bg, "below the panel is untouched");
    }

    #[test]
    fn draw_console_missing_conback_fills_dark_no_panic() {
        let pal = ramp_palette();
        let cc = lit_conchars();
        let bg = [200u8, 200, 200];
        let mut c = Console::new();
        c.toggle();
        c.println("text");
        let mut img = Image::new(320, 200, bg);
        // Missing conback => a dark fill rectangle, not a panic, not the bg.
        draw_console(&mut img, &c, None, Some(&cc), &pal, 0.0);
        assert_ne!(img.rgb[2 * img.w + 2], bg, "missing conback still fills the panel");

        // Missing conchars => the background still draws, text is skipped, no panic.
        let conback = solid_pic(320, 200, 5);
        let mut img2 = Image::new(320, 200, bg);
        draw_console(&mut img2, &c, Some(&conback), None, &pal, 0.0);
        assert_eq!(img2.rgb[2 * img2.w + 2], pal[5], "background draws without conchars");

        // A tiny framebuffer must not panic either.
        let mut tiny = Image::new(1, 1, bg);
        draw_console(&mut tiny, &c, Some(&conback), Some(&cc), &pal, 0.0);
        let mut zero = Image::new(0, 0, bg);
        draw_console(&mut zero, &c, Some(&conback), Some(&cc), &pal, 0.0);
    }

    // -- Frustum culling (R_CullBox) ---------------------------------------

    #[test]
    fn frustum_culls_box_behind_camera_keeps_box_in_front() {
        // Camera at the origin looking down +X (yaw 0, pitch 0), 90-deg fov.
        let cam = Camera { pos: [0.0, 0.0, 0.0], yaw: 0.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let frustum = Frustum::from_camera(&cam, 320, 200);

        // A box entirely BEHIND the camera (negative X): fully outside the near
        // plane -> culled.
        assert!(
            frustum.culls([-200.0, -10.0, -10.0], [-100.0, 10.0, 10.0]),
            "a box wholly behind the camera must be culled"
        );

        // A box straddling the near plane (spanning x = -10..50 around the eye):
        // touches the view -> NOT culled (conservative).
        assert!(
            !frustum.culls([-10.0, -10.0, -10.0], [50.0, 10.0, 10.0]),
            "a box straddling the near plane must NOT be culled"
        );

        // A box well in FRONT and centred on the view axis -> NOT culled.
        assert!(
            !frustum.culls([90.0, -10.0, -10.0], [110.0, 10.0, 10.0]),
            "a box in front of the camera must NOT be culled"
        );

        // A box far off to the LEFT (large +Y, beyond the 90-deg side at this
        // depth) is fully outside the left side plane -> culled. At x=100 the
        // left frustum edge is y=100 (tan45); a box at y in [500,600] is outside.
        assert!(
            frustum.culls([100.0, 500.0, -10.0], [110.0, 600.0, 10.0]),
            "a box outside the side frustum plane must be culled"
        );
    }

    #[test]
    fn frustum_never_culls_a_box_that_encloses_the_eye() {
        // A huge box around the camera straddles every plane -> never culled,
        // guaranteeing we never punch a hole when geometry surrounds the view.
        let cam = Camera { pos: [10.0, 20.0, 30.0], yaw: 35.0, pitch: -12.0, roll: 0.0, fov_deg: 90.0 };
        let frustum = Frustum::from_camera(&cam, 640, 480);
        assert!(
            !frustum.culls([-1000.0, -1000.0, -1000.0], [1000.0, 1000.0, 1000.0]),
            "a box enclosing the eye must never be culled"
        );
    }

    // -- Lightmap surface cache --------------------------------------------

    /// Clear both thread-local caches so a test starts from a known state
    /// (tests share a thread, and a prior test may have populated them).
    fn reset_render_caches() {
        GEOM_CACHE.with(|c| *c.borrow_mut() = None);
        LIGHT_CACHE.with(|c| *c.borrow_mut() = None);
        SURF_CACHE.with(|c| *c.borrow_mut() = None);
    }

    #[test]
    fn lightmap_cache_returns_bit_identical_luxels_for_same_style_key() {
        reset_render_caches();
        // A 2-style face: styles[0]=0 (steady), styles[1]=1 (animated). The owned
        // combine is cacheable (not a static borrow).
        let (bsp, face, poly) = two_style_face_bsp([0, 1, 255, 255], 100, 200);
        let mut scales = NEUTRAL_LIGHTSTYLE_SCALES;
        scales[1] = 0.5;

        // First call: MISS -> builds + caches. Second call (same key): HIT.
        let first =
            face_lightmap_world_cached(&bsp, 0, &face, &poly, &scales, &[], 0).expect("present");
        let second =
            face_lightmap_world_cached(&bsp, 0, &face, &poly, &scales, &[], 0).expect("present");

        // The cached luxels must be BIT-identical to a fresh, cache-free build.
        let fresh = face_lightmap_dyn(&bsp, &face, &poly, &scales, &[], 0).expect("present");
        let (lf, ls, lfresh) = match (&first.luxels, &second.luxels, &fresh.luxels) {
            (Luxels::Owned(a), Luxels::Owned(b), Luxels::Owned(c)) => (a, b, c),
            _ => panic!("a 2-style face must own the combined buffer"),
        };
        assert_eq!(lf.len(), lfresh.len());
        for i in 0..lf.len() {
            assert_eq!(lf[i].to_bits(), lfresh[i].to_bits(), "first build vs fresh luxel {i}");
            assert_eq!(ls[i].to_bits(), lfresh[i].to_bits(), "cached HIT vs fresh luxel {i}");
        }
        assert_eq!((second.lmw, second.lmh), (fresh.lmw, fresh.lmh));
        assert_eq!(second.texmins, fresh.texmins);
    }

    #[test]
    fn lightmap_cache_rebuilds_when_style_key_changes() {
        reset_render_caches();
        let (bsp, face, poly) = two_style_face_bsp([0, 1, 255, 255], 100, 200);

        // Build at scale 0.5, then again at scale 1.0 (a torch ticking). The
        // second result must reflect the NEW scale, not the stale cached one.
        let mut s_half = NEUTRAL_LIGHTSTYLE_SCALES;
        s_half[1] = 0.5;
        let half =
            face_lightmap_world_cached(&bsp, 0, &face, &poly, &s_half, &[], 0).expect("present");

        let mut s_full = NEUTRAL_LIGHTSTYLE_SCALES;
        s_full[1] = 1.0;
        let full =
            face_lightmap_world_cached(&bsp, 0, &face, &poly, &s_full, &[], 0).expect("present");

        // Compare against fresh builds at each scale.
        let fresh_full = face_lightmap_dyn(&bsp, &face, &poly, &s_full, &[], 0).expect("present");
        match (&half.luxels, &full.luxels, &fresh_full.luxels) {
            (Luxels::Owned(h), Luxels::Owned(f), Luxels::Owned(ff)) => {
                // block0=100, block1=200: half -> 100+0.5*200=200; full ->
                // 100+1.0*200=300. They must differ, and `full` must match a
                // fresh full build bit-for-bit.
                assert_ne!(h[0].to_bits(), f[0].to_bits(), "changed style key must rebuild");
                assert!((h[0] - 200.0).abs() < 1e-4, "half-scale luxel = 200, got {}", h[0]);
                assert!((f[0] - 300.0).abs() < 1e-4, "full-scale luxel = 300, got {}", f[0]);
                for i in 0..f.len() {
                    assert_eq!(f[i].to_bits(), ff[i].to_bits(), "rebuilt full vs fresh luxel {i}");
                }
            }
            _ => panic!("2-style face must own the combine"),
        }
    }

    #[test]
    fn lightmap_cache_invalidates_when_faces_len_changes() {
        reset_render_caches();
        // World A: a 2-style face, populate the cache for face 0.
        let (bsp_a, face_a, poly) = two_style_face_bsp([0, 1, 255, 255], 100, 200);
        let mut scales = NEUTRAL_LIGHTSTYLE_SCALES;
        scales[1] = 0.5;
        let _ = face_lightmap_world_cached(&bsp_a, 0, &face_a, &poly, &scales, &[], 0).expect("present");

        // World B: a DIFFERENT world with a different faces.len() and different
        // lightmap bytes at face 0. The fingerprint mismatch must clear the
        // (face-index-keyed) cache so face 0 is rebuilt from B's data, NOT served
        // from A's stale entry.
        let (mut bsp_b, face_b, poly_b) = two_style_face_bsp([0, 1, 255, 255], 40, 240);
        // Make faces.len() differ from A (A had 1 face) so the fingerprint flips
        // via the faces_len field as well as the &Bsp pointer.
        bsp_b.faces.push(face_b.clone());
        bsp_b.faces.push(face_b.clone());
        assert_ne!(bsp_a.faces.len(), bsp_b.faces.len());

        let got =
            face_lightmap_world_cached(&bsp_b, 0, &face_b, &poly_b, &scales, &[], 0).expect("present");
        let fresh_b = face_lightmap_dyn(&bsp_b, &face_b, &poly_b, &scales, &[], 0).expect("present");
        match (&got.luxels, &fresh_b.luxels) {
            (Luxels::Owned(g), Luxels::Owned(fb)) => {
                // B's block0=40, block1=240, scale 0.5 -> 40+120=160 (NOT A's 200).
                assert!((g[0] - 160.0).abs() < 1e-4, "world B luxel = 160, got {} (stale A?)", g[0]);
                for i in 0..g.len() {
                    assert_eq!(g[i].to_bits(), fb[i].to_bits(), "world B rebuilt vs fresh luxel {i}");
                }
            }
            _ => panic!("2-style face must own the combine"),
        }
    }

    #[test]
    fn lightmap_cache_dlit_face_rebuilds_each_frame_and_keeps_dlight() {
        reset_render_caches();
        // A single steady style-0 face at neutral scale -> normally a static
        // borrow (uncached). A reaching dlight must still produce the owned,
        // dlit buffer (NOT a cached static-only buffer) every call.
        let (bsp, face, poly) = one_face_bsp_zplane(100);
        let dl = DynamicLight::new([0.0, 0.0, 16.0], 60.0, 10.0, 0.0, 0.0, 0);

        // First, populate any cache via a dlight-free neutral call (static borrow,
        // not cached). Then a reaching dlight: must own the buffer and brighten.
        let _ = face_lightmap_world_cached(&bsp, 0, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, &[], 0);
        let lit = face_lightmap_world_cached(
            &bsp, 0, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&dl),
            ALL_DLIGHT_BITS,
        )
        .expect("present");
        assert!(matches!(lit.luxels, Luxels::Owned(_)), "a dlit face must own the dlit buffer");
        // Must match the direct (cache-free) dlit build exactly.
        let fresh = face_lightmap_dyn(
            &bsp, &face, &poly, &NEUTRAL_LIGHTSTYLE_SCALES, std::slice::from_ref(&dl),
            ALL_DLIGHT_BITS,
        )
        .expect("present");
        match (&lit.luxels, &fresh.luxels) {
            (Luxels::Owned(a), Luxels::Owned(b)) => {
                for i in 0..a.len() {
                    assert_eq!(a[i].to_bits(), b[i].to_bits(), "dlit cached-path vs fresh luxel {i}");
                }
            }
            _ => unreachable!(),
        }
    }

    /// A `demo_room` whose every face is a 2-style lightmapped wall (styles
    /// `[0, 1]`) pointing into a uniform lighting lump. Rendering this with a
    /// non-neutral style-1 scale forces the OWNED multi-style combine on every
    /// face -> exercises the lightmap surface cache end-to-end.
    fn lightmapped_demo_room(block0: u8, block1: u8) -> Bsp {
        let mut bsp = demo_room();
        // Two concatenated blocks per face, uniform so any face's grid (whatever
        // its extents) reads well-defined bytes. Lump is large enough for the
        // biggest face's 2*lmw*lmh.
        let mut lighting = vec![block0; 200_000];
        for b in lighting.iter_mut().skip(100_000) {
            *b = block1;
        }
        bsp.lighting = lighting;
        for f in bsp.faces.iter_mut() {
            f.lightofs = 0;
            f.styles = [0, 1, 255, 255];
        }
        bsp
    }

    #[test]
    fn world_render_is_pixel_identical_across_frames_with_lightmap_cache() {
        // This mirrors what the orchestrator diffs: render the SAME lightmapped,
        // style-animated world repeatedly and require byte-identical pixels. The
        // first render populates the geom + lightmap caches; the second hits
        // them. The cached combine must be bit-identical, so the images match.
        reset_render_caches();
        let bsp = lightmapped_demo_room(100, 200);
        let pal = [[180u8, 150, 90]; 256];
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let mut styles = NEUTRAL_LIGHTSTYLE_SCALES;
        styles[1] = 0.5; // non-neutral -> owned combine -> cache used

        let render = |b: &Bsp| {
            render_scene_ext(
                b, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &styles, None,
            )
        };

        let frame1 = render(&bsp); // populates caches
        let frame2 = render(&bsp); // cache hits
        assert_eq!(frame1.rgb, frame2.rgb, "cached frame must be pixel-identical to the first");

        // A change in the style scale must change the cache key AND the pixels
        // (proving the cache is keyed on the scale, not stale).
        let mut styles_b = NEUTRAL_LIGHTSTYLE_SCALES;
        styles_b[1] = 1.0;
        let frame_b = render_scene_ext(
            &bsp, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &styles_b, None,
        );
        assert_ne!(
            frame1.rgb, frame_b.rgb,
            "a different style scale must rebuild and produce different pixels"
        );

        // Re-render at the ORIGINAL scale: must again equal frame1 (the cache
        // correctly rebuilt back to the 0.5 key).
        let frame3 = render(&bsp);
        assert_eq!(frame1.rgb, frame3.rgb, "returning to the original key reproduces frame1");
    }

    /// Give every face a plain (non-special) wall texture so the lit-surface cache
    /// (which needs a real miptex + colormap) is actually exercised.
    fn demo_room_with_walls(mut bsp: Bsp) -> Bsp {
        let n_tex = bsp.texinfo.len().max(1);
        bsp.textures = (0..n_tex)
            .map(|i| {
                Some(crate::bsp::MipTex {
                    name: format!("wall{i}"),
                    width: 16,
                    height: 16,
                    offsets: [0, 0, 0, 0],
                    pixels: vec![(i * 7) as u8; 16 * 16],
                    anim: None,
                })
            })
            .collect();
        bsp
    }

    #[test]
    fn external_models_bypass_and_dont_evict_world_surf_cache() {
        // REGRESSION (performance): external brush models (the b_*.bsp item boxes)
        // re-clone their Bsp every frame, so each has a different WorldFingerprint
        // every frame and every instance — they can NEVER hit the surface cache.
        // They must therefore BYPASS it: routing them through the shared world slot
        // would evict the world's resident cache and reintroduce the ~60ms/frame
        // re-bake-everything cost. Here we warm the world cache, then render a frame
        // containing MANY (26 > the old broken 24-slot LRU) distinct external models,
        // and assert (a) the externals baked (went through the bypass, not the cache)
        // and (b) the world cache is completely untouched afterwards.
        reset_render_caches();
        let world = demo_room_with_walls(lightmapped_demo_room(100, 200));
        // 26 distinct external "boxes" (each a separate Bsp -> distinct fingerprint),
        // standing at the world origin so their inward walls are in view and drawn.
        let ext_bsps: Vec<Bsp> = (0..26)
            .map(|k| demo_room_with_walls(lightmapped_demo_room(40 + k as u8, 220)))
            .collect();
        let externals: Vec<ExternalBModel> =
            ext_bsps.iter().map(|b| ExternalBModel { bsp: b, origin: [0.0, 0.0, 0.0] }).collect();

        let pal = [[180u8, 150, 90]; 256];
        let colormap = vec![0u8; COLORMAP_LEN]; // present -> the surf-cache path is active
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let mut styles = NEUTRAL_LIGHTSTYLE_SCALES;
        styles[1] = 0.5;
        let render = |ext: &[ExternalBModel]| {
            render_scene_ext(
                &world, &cam, 160, 120, &pal, &[], &[], ext, None, 0.0, &[], &[], &styles,
                Some(&colormap),
            )
        };

        let _ = render(&[]); // bake the world's surface blocks into the cache

        // Sanity: the world scene hits its warm cache (else the asserts below are vacuous).
        render_stats_begin();
        let _ = render(&[]);
        let warm = render_stats_end();
        assert!(
            warm.surf_cache_hits > 0 && warm.surf_baked == 0,
            "world scene must hit the warm surf cache (got {} hits, {} bakes)",
            warm.surf_cache_hits, warm.surf_baked
        );
        let world_hits = warm.surf_cache_hits;

        // A frame with 26 distinct external models: the world still fully hits, and
        // the externals BAKE (proving they took the bypass path, not the cache).
        render_stats_begin();
        let _ = render(&externals);
        let with_ext = render_stats_end();
        assert_eq!(
            with_ext.surf_cache_hits, world_hits,
            "the world's faces must all still hit while externals draw (got {} vs {})",
            with_ext.surf_cache_hits, world_hits
        );
        assert_eq!(
            with_ext.surf_baked, 0,
            "the world must NOT re-bake while externals draw — got {} cached-path bakes",
            with_ext.surf_baked
        );
        assert!(
            with_ext.surf_bypass_baked > 0,
            "external models must bake via the bypass path — got {} bypass bakes",
            with_ext.surf_bypass_baked
        );

        // THE GUARD: after that external-laden frame, the world cache is untouched —
        // a subsequent world-only frame still hits everything, zero re-bakes. (With
        // the old shared/LRU cache the externals would have evicted it -> re-bakes.)
        render_stats_begin();
        let _ = render(&[]);
        let after = render_stats_end();
        assert_eq!(
            after.surf_baked, 0,
            "world cache was polluted/evicted by external models — {} faces re-baked \
             (the ~60ms/frame regression)",
            after.surf_baked
        );
        assert_eq!(after.surf_cache_hits, world_hits, "world cache must be fully intact");
    }

    #[test]
    fn world_render_unaffected_by_intervening_different_world() {
        // Render world A, then a DIFFERENT world B (different geometry + lighting,
        // which resets the face-index-keyed caches), then world A again. The two
        // renders of A must be byte-identical — proving the changelevel
        // invalidation never serves B's cached data for A's faces.
        reset_render_caches();
        let a = lightmapped_demo_room(100, 200);
        let b = lightmapped_demo_room(60, 240); // different lighting bytes
        let pal = [[180u8, 150, 90]; 256];
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let mut styles = NEUTRAL_LIGHTSTYLE_SCALES;
        styles[1] = 0.5;
        let render = |bsp: &Bsp| {
            render_scene_ext(
                bsp, &cam, 160, 120, &pal, &[], &[], &[], None, 0.0, &[], &[], &styles, None,
            )
        };

        let a1 = render(&a);
        let _b = render(&b); // resets caches to world B
        let a2 = render(&a); // must rebuild A's caches, not reuse B's
        assert_eq!(a1.rgb, a2.rgb, "world A renders identically before and after world B");
    }

    #[test]
    fn frustum_culled_faces_draw_no_onscreen_pixel() {
        // CONSERVATIVENESS on a concrete scene: for every world-model face the
        // frustum CULLS, confirm it could not have contributed any on-screen
        // pixel — i.e. all its (in-front-of-near) projected vertices fall outside
        // the framebuffer rectangle. This is the property that guarantees the
        // cull never punches a hole (removes a visible pixel) vs. the pre-cull
        // renderer.
        reset_render_caches();
        let bsp = lightmapped_demo_room(100, 200);
        let (w, h) = (160usize, 120usize);
        // A camera tucked in a corner looking along an axis so a good chunk of
        // the room's faces fall outside the view (some get culled).
        let cam = Camera { pos: [-240.0, -240.0, 20.0], yaw: 10.0, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
        let frustum = Frustum::from_camera(&cam, w, h);

        let (forward, right, up) = cam.basis();
        let cx = w as f32 / 2.0;
        let cy = h as f32 / 2.0;
        let tan_half = (cam.fov_deg as f64 * 0.5).to_radians().tan();
        let focal = if tan_half.abs() < 1e-6 { cx } else { (cx as f64 / tan_half) as f32 };

        let mut culled = 0usize;
        for (idx, face) in bsp.faces.iter().enumerate() {
            let geom = face_geom_cached(&bsp, idx, face);
            if geom.bad {
                continue;
            }
            if !frustum.culls(geom.mins, geom.maxs) {
                continue;
            }
            culled += 1;
            // A culled face must not project any vertex into the screen rect.
            // (Vertices behind the near plane never draw; the near plane is one of
            // the cull planes, and a face culled by a SIDE plane lies wholly to
            // that side, so every in-front vertex is off-screen on that side.)
            for v in geom.poly.iter() {
                let rel = sub(*v, cam.pos);
                let vz = dot(rel, forward);
                if vz <= NEAR_PLANE {
                    continue; // behind near -> never rasterised
                }
                let sx = cx + focal * dot(rel, right) / vz;
                let sy = cy - focal * dot(rel, up) / vz;
                let onscreen = sx >= 0.0 && sx < w as f32 && sy >= 0.0 && sy < h as f32;
                assert!(
                    !onscreen,
                    "culled face {idx} projects vertex on-screen at ({sx},{sy}) — would punch a hole"
                );
            }
        }
        assert!(culled > 0, "the test camera should cull at least one face to be meaningful");
    }

    #[test]
    fn face_geom_cache_matches_uncached_build_and_invalidates() {
        reset_render_caches();
        let (bsp, face, _poly) = two_style_face_bsp([0, 1, 255, 255], 100, 200);
        // First call builds + caches; second returns the cached clone.
        let g1 = face_geom_cached(&bsp, 0, &face);
        let g2 = face_geom_cached(&bsp, 0, &face);
        assert!(!g1.bad);
        // The cached poly must equal a direct face_world_poly reconstruction
        // (face_world_poly walks the BSP edge tables, so this — not the helper's
        // literal `poly` used for lightmap math — is the geometry the loop sees).
        let mut direct = Vec::new();
        assert!(face_world_poly(&bsp, &face, &mut direct));
        assert_eq!(*g1.poly, direct);
        assert_eq!(*g2.poly, direct);
        // Normal + centroid match a direct compute.
        assert_eq!(g1.normal, face_normal(&bsp, &face));
        // AABB encloses every vertex.
        for v in &direct {
            for (k, &c) in v.iter().enumerate() {
                assert!(g1.mins[k] <= c && c <= g1.maxs[k]);
            }
        }
        // A different world (more faces) invalidates: still a correct rebuild.
        let (mut bsp_b, face_b, _polyb) = two_style_face_bsp([0, 255, 255, 255], 50, 50);
        bsp_b.faces.push(face_b.clone());
        let gb = face_geom_cached(&bsp_b, 0, &face_b);
        assert!(!gb.bad);
    }

    // -- SCR_CalcRefdef / R_SetVrect / Draw_TileClear / sb_lines -------------

    fn vr(x: usize, y: usize, w: usize, h: usize) -> ViewRect {
        ViewRect { x, y, w, h }
    }

    #[test]
    fn calc_refdef_matches_the_c_at_320x200() {
        // viewsize 100: the full width ABOVE the 48-line status bar — not a
        // full-screen view with the bar pasted over its bottom (the old bug:
        // horizon at y=100 instead of 76, 48 rows rendered only to be covered).
        let r = calc_refdef(320, 200, 100.0, false);
        assert_eq!((r.vrect, r.sb_lines), (vr(0, 0, 320, 152), 48));
        // 110: no inventory strip -> 24 lines, the view grows to 176.
        let r = calc_refdef(320, 200, 110.0, false);
        assert_eq!((r.vrect, r.sb_lines), (vr(0, 0, 320, 176), 24));
        // 120: no status bar at all -> the whole screen.
        let r = calc_refdef(320, 200, 120.0, false);
        assert_eq!((r.vrect, r.sb_lines), (vr(0, 0, 320, 200), 0));
        // 50: half size, centred horizontally on the screen and vertically in
        // the 152 rows above the bar: x = (320-160)/2, y = (152-100)/2.
        let r = calc_refdef(320, 200, 50.0, false);
        assert_eq!((r.vrect, r.sb_lines), (vr(80, 26, 160, 100), 48));
        // 30, the minimum: exactly the 96-wide "min for icons".
        let r = calc_refdef(320, 200, 30.0, false);
        assert_eq!(r.vrect, vr(112, 46, 96, 60));
        // 70: (int)(320 * 0.7f) = 224 (& ~7 = 224), (int)(200 * 0.7f) = 140.
        let r = calc_refdef(320, 200, 70.0, false);
        assert_eq!(r.vrect, vr(48, 6, 224, 140));
        // 90: 288x180 would overlap the bar -> clipped to the 152 rows above it.
        let r = calc_refdef(320, 200, 90.0, false);
        assert_eq!(r.vrect, vr(16, 0, 288, 152));
    }

    #[test]
    fn calc_refdef_bounds_viewsize_and_goes_full_screen_for_intermission() {
        // SCR_CalcRefdef clamps viewsize to 30..=120.
        assert_eq!(calc_refdef(320, 200, 5.0, false), calc_refdef(320, 200, 30.0, false));
        assert_eq!(calc_refdef(320, 200, 500.0, false), calc_refdef(320, 200, 120.0, false));
        assert_eq!(calc_refdef(320, 200, f32::NAN, false), calc_refdef(320, 200, 100.0, false));
        // "intermission is always full screen": any viewsize, no status bar.
        for vs in [30.0, 50.0, 100.0, 110.0, 120.0] {
            let r = calc_refdef(320, 200, vs, true);
            assert_eq!((r.vrect, r.sb_lines), (vr(0, 0, 320, 200), 0), "viewsize {vs}");
        }
    }

    #[test]
    fn calc_refdef_scales_the_status_bar_with_the_2d_layer() {
        // The port's 2-D layer is the 320x200 screen scaled by w/320, so the
        // view clears exactly the rows draw_hud_into paints: 48*scale.
        assert_eq!(calc_refdef(960, 600, 100.0, false).vrect, vr(0, 0, 960, 456));
        assert_eq!(calc_refdef(480, 300, 100.0, false).vrect, vr(0, 0, 480, 228));
        assert_eq!(calc_refdef(1120, 700, 110.0, false).vrect, vr(0, 0, 1120, 616));
        assert_eq!(calc_refdef(1280, 800, 120.0, false).vrect, vr(0, 0, 1280, 800));
        // 960x600 at 50: 480x300 centred above the 144-row bar.
        assert_eq!(calc_refdef(960, 600, 50.0, false).vrect, vr(240, 78, 480, 300));
        // Every preset at every step stays inside the frame and above the bar.
        for &(w, h) in RESOLUTION_PRESETS.iter() {
            for step in 3..=12 {
                let r = calc_refdef(w as usize, h as usize, step as f32 * 10.0, false);
                let bar = (r.sb_lines as f32 * w as f32 / 320.0).ceil() as usize;
                assert!(r.vrect.x + r.vrect.w <= w as usize);
                assert!(r.vrect.y + r.vrect.h + bar <= h as usize, "{w}x{h} @ {step}0");
                assert_eq!(r.vrect.w % 8, 0);
                assert_eq!(r.vrect.h % 2, 0);
            }
        }
        // A degenerate frame never panics or escapes the bounds.
        let r = calc_refdef(8, 4, 30.0, false);
        assert!(r.vrect.x + r.vrect.w <= 8 && r.vrect.y + r.vrect.h <= 4);
        let _ = calc_refdef(0, 0, 100.0, false);
    }

    /// A 64x64 backtile whose texel (x, y) is palette index `(x + 64*y) % 251`,
    /// so any sampling error shows up as the wrong colour.
    fn test_backtile() -> Qpic {
        let data = (0..64 * 64).map(|i| (i % 251) as u8).collect();
        Qpic { width: 64, height: 64, data }
    }

    #[test]
    fn draw_tile_clear_tiles_from_the_screen_origin() {
        let pal = ramp_palette();
        let tile = test_backtile();
        let at = |x: usize, y: usize| pal[tile.data[(y % 64) * 64 + x % 64] as usize];
        // Scale 1 (320 wide): texel (x mod 64, y mod 64) — anchored at the
        // SCREEN origin, not the rectangle's corner (Draw_TileClear's offsets).
        let mut img = Image::new(320, 200, [7, 7, 7]);
        draw_tile_clear(&mut img, Some(&tile), 70, 30, 100, 50, &pal);
        assert_eq!(img.rgb[30 * 320 + 70], at(70, 30));
        assert_eq!(img.rgb[79 * 320 + 169], at(169, 79));
        assert_eq!(img.rgb[29 * 320 + 70], [7, 7, 7], "outside the rect untouched");
        assert_eq!(img.rgb[30 * 320 + 170], [7, 7, 7], "outside the rect untouched");
        // Scale 2 (640 wide): each texel covers 2x2 pixels, like the rest of
        // the scaled 2-D layer.
        let mut big = Image::new(640, 400, [7, 7, 7]);
        draw_tile_clear(&mut big, Some(&tile), 0, 0, 640, 400, &pal);
        for &(x, y) in &[(0, 0), (1, 1), (129, 3), (300, 250), (639, 399)] {
            assert_eq!(big.rgb[y * 640 + x], at(x / 2, y / 2), "({x},{y})");
        }
        // No tile: black, never a panic; an off-frame rect is a no-op.
        let mut img2 = Image::new(32, 32, [7, 7, 7]);
        draw_tile_clear(&mut img2, None, 0, 0, 32, 32, &pal);
        assert!(img2.rgb.iter().all(|&p| p == [0, 0, 0]));
        draw_tile_clear(&mut img2, Some(&tile), 40, 40, 10, 10, &pal);
    }

    #[test]
    fn compose_view_places_the_view_inside_a_backtile_border() {
        let pal = ramp_palette();
        let tile = test_backtile();
        // viewsize 50 at 320x200: a 160x100 view at (80, 26).
        let r = calc_refdef(320, 200, 50.0, false);
        let view = Image::new(r.vrect.w, r.vrect.h, [250, 1, 2]);
        let img = compose_view(view, r.vrect, 320, 200, Some(&tile), &pal);
        assert_eq!((img.w, img.h), (320, 200));
        let tile_at = |x: usize, y: usize| pal[tile.data[(y % 64) * 64 + x % 64] as usize];
        for y in 0..200 {
            for x in 0..320 {
                let inside = (80..240).contains(&x) && (26..126).contains(&y);
                let want = if inside { [250, 1, 2] } else { tile_at(x, y) };
                assert_eq!(img.rgb[y * 320 + x], want, "({x},{y})");
            }
        }
        // A full-screen view (viewsize 120) passes through untouched.
        let full = calc_refdef(320, 200, 120.0, false);
        let view = Image::new(320, 200, [250, 1, 2]);
        let out = compose_view(view, full.vrect, 320, 200, Some(&tile), &pal);
        assert!(out.rgb.iter().all(|&p| p == [250, 1, 2]));
    }

    /// A gfx.wad with the three status-bar strips as solid colours: `sbar`
    /// (index 1), `ibar` (2) and `scorebar` (3).
    fn build_sbar_strips_wad() -> Wad2 {
        let pics: Vec<(String, Vec<u8>)> = vec![
            ("sbar".to_string(), qpic_payload(320, 24, 1)),
            ("ibar".to_string(), qpic_payload(320, 24, 2)),
            ("scorebar".to_string(), qpic_payload(320, 24, 3)),
        ];
        let mut payloads = Vec::new();
        let mut offsets = Vec::new();
        let mut pos = WADINFO_SIZE;
        for (_, p) in &pics {
            offsets.push(pos);
            payloads.extend_from_slice(p);
            pos += p.len();
        }
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"WAD2");
        bytes.extend_from_slice(&(pics.len() as i32).to_le_bytes());
        bytes.extend_from_slice(&(pos as i32).to_le_bytes());
        bytes.extend_from_slice(&payloads);
        let mut dir = Vec::new();
        for ((name, p), &off) in pics.iter().zip(offsets.iter()) {
            push_lump(&mut dir, off as i32, p.len() as i32, name);
        }
        bytes.extend_from_slice(&dir);
        Wad2::parse(bytes).expect("synthetic strips wad parses")
    }

    #[test]
    fn draw_hud_follows_sb_lines_like_sbar_draw() {
        let wad = build_sbar_strips_wad();
        let pal = ramp_palette();
        let fill = [42u8, 42, 42];
        let draw = |sb_lines: i32, health: i32| {
            let mut img = Image::new(320, 200, fill);
            let hud = Hud {
                wad: &wad,
                palette: &pal,
                health,
                ammo: 0,
                armor: 0,
                items: 0,
                weapon: 0,
                ammo_shells: 0,
                ammo_nails: 0,
                ammo_rockets: 0,
                ammo_cells: 0,
                time: 0.0,
                monsters: 0,
                total_monsters: 0,
                secrets: 0,
                total_secrets: 0,
                level_name: "",
                show_scores: false,
                sb_lines,
            };
            draw_hud_into(&mut img, &hud);
            img
        };
        let ibar_row = 160 * 320 + 5; // inside rows 152..176
        let sbar_row = 190 * 320 + 5; // inside rows 176..200
        // 48 lines (viewsize <= 100): inventory strip over the status strip.
        let img = draw(48, 100);
        assert_eq!((img.rgb[ibar_row], img.rgb[sbar_row]), (pal[2], pal[1]));
        assert_eq!(img.rgb[151 * 320 + 5], fill, "nothing above the 48 lines");
        // 24 lines (viewsize 110): the status strip alone.
        let img = draw(24, 100);
        assert_eq!((img.rgb[ibar_row], img.rgb[sbar_row]), (fill, pal[1]));
        // 0 lines (viewsize 120): no status bar at all.
        let img = draw(0, 100);
        assert!(img.rgb.iter().all(|&p| p == fill), "sb_lines 0 draws nothing");
        // ...except the death scoreboard, which Sbar_Draw shows regardless.
        let img = draw(0, 0);
        assert_eq!((img.rgb[ibar_row], img.rgb[sbar_row]), (fill, pal[3]));
        let img = draw(48, 0);
        assert_eq!((img.rgb[ibar_row], img.rgb[sbar_row]), (pal[2], pal[3]));
    }
}
