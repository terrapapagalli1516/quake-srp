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
use alias::{draw_alias_model, draw_viewmodel};
use raster::{hash_color, raster_triangle, Projected};
use sky::resolve_sky_spans;
use stats::{stat, stats_on, StatInstant};
use warp::TurbTable;
use world::{draw_submodel, draw_world_textured};

mod view;
mod raster;
mod light;
mod surf;
mod warp;
mod sky;
mod vis;
mod world;
mod alias;
mod polyse;
mod stats;
#[cfg(test)]
mod fixtures;

pub use alias::{ModelInstance, Viewmodel};
pub use light::{LIGHTSTYLES, NEUTRAL_LIGHTSTYLE_SCALES};
pub use stats::{render_stats_begin, render_stats_end, set_render_stats_clock, RenderStats};
pub use view::{
    apply_blend, build_gamma_table, combine_cshifts, content_cshift, powerup_cshift, view_bob,
    viewmodel_angles, viewmodel_fudge, viewmodel_origin_ofs,
};
pub use vis::point_in_leaf;
pub use warp::apply_warp;
pub use world::{draw_brush_bsp, BModelInstance, ExternalBModel};

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
    resolve_sky_spans(&mut image, &zbuf, bsp, palette);
    image
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
/// still wins (its 1/z is tripled, as `R_AliasDrawModel` does). Passing an
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
    // Phase wall-timers: `StatInstant::now()` is only evaluated when the profiler is
    // on (via `.then(..)`), so the shared render path never reads a clock. (On wasm,
    // where `Instant` is unavailable, only the opt-in benchmark build turns the
    // profiler on, after installing a JS clock via `set_render_stats_clock`.)
    let tw = stats_on().then(StatInstant::now);
    draw_world_textured(&mut image, &mut zbuf, bsp, cam, palette, &turb, time, light_styles, dlights, colormap);
    if let Some(t) = tw { stat(|s| s.world_ns += t.elapsed().as_nanos() as u64); }
    let ts = stats_on().then(StatInstant::now);
    for bm in bmodels {
        // Inline submodels share the world `bsp`, so their surface blocks ARE cached.
        draw_submodel(&mut image, &mut zbuf, bsp, cam, palette, bm.model_index, bm.origin, &turb, time, light_styles, dlights, colormap, bm.frame, true);
    }
    if let Some(t) = ts { stat(|s| s.submodel_ns += t.elapsed().as_nanos() as u64); }
    let te = stats_on().then(StatInstant::now);
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
    // The sky, span by span, now that every brush surface that can cover it has
    // been drawn (id: `D_DrawSkyScans8` inside `D_DrawSurfaces`, before entities).
    resolve_sky_spans(&mut image, &zbuf, bsp, palette);
    let ta = stats_on().then(StatInstant::now);
    for inst in models {
        draw_alias_model(&mut image, &mut zbuf, bsp, cam, inst, palette, dlights, light_styles, time, colormap);
    }
    if let Some(t) = ta { stat(|s| s.alias_ns += t.elapsed().as_nanos() as u64); }
    // Particles draw after the world/models, z-tested against the same buffer so
    // walls occlude them. (id draws them after the gun; with the gun's tripled
    // 1/z in the shared z-buffer the order only matters on exact ties.)
    let tp = stats_on().then(StatInstant::now);
    draw_particles(&mut image, &mut zbuf, cam, particles, palette, w, h);
    if let Some(t) = tp { stat(|s| s.particle_ns += t.elapsed().as_nanos() as u64); }
    // Sprite-model entities (explosion flash, bubbles) — camera-facing billboards,
    // z-tested against the same buffer, drawn after models and before the viewmodel.
    let tsp = stats_on().then(StatInstant::now);
    draw_sprites(&mut image, &mut zbuf, cam, sprites, palette, time, w, h);
    if let Some(t) = tsp { stat(|s| s.sprite_ns += t.elapsed().as_nanos() as u64); }
    // The weapon: R_DrawViewModel, after the entities.
    let tv = stats_on().then(StatInstant::now);
    if let Some(vm) = viewmodel {
        draw_viewmodel(&mut image, &mut zbuf, bsp, cam, &vm, palette, dlights, light_styles, time, colormap);
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
    /// (default.cfg's viewsize, gamma, volume, sensitivity and bindings); the
    /// host reads them live each frame. The video mode is not in default.cfg.
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

    /// "Reset to defaults" = `exec default.cfg`, and exactly what that file
    /// sets: `unbindall` + its `bind` lines (the key table), and the four
    /// "default cvars" at its end — `viewsize 100`, `gamma 1.0`, `volume 0.7`,
    /// `sensitivity 3`. Nothing else: CD Music Volume, Always Run
    /// (`cl_forwardspeed`), Invert Mouse (`m_pitch`), Lookspring and
    /// Lookstrafe keep their values, as in WinQuake, and so does the video mode.
    pub fn reset_defaults(&mut self) {
        self.viewsize = VIEWSIZE_DEFAULT;
        self.gamma = GAMMA_DEFAULT;
        self.volume = VOLUME_DEFAULT;
        self.sensitivity = SENS_DEFAULT;
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

/// `M_Print` (menu.c): menu text in the conchars' second, bronze half — each
/// character is drawn as cell `c + 128` — at virtual `(vx, vy)`, 8 px apart.
/// (`M_PrintWhite` is plain [`draw_string_scaled`].) The menus print their
/// labels, values and hints this way; white marks only the odd highlight (the
/// current video mode, "No Communications Available").
#[allow(clippy::too_many_arguments)]
fn m_print(
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
    for (i, b) in text.bytes().enumerate() {
        let x = vx + 8.0 * i as f32;
        draw_char_scaled(image, conchars, x, vy, b.wrapping_add(128), scale, ox, oy, palette);
    }
}

/// `Draw_FadeScreen` (draw.c), which `M_Draw` runs under every menu drawn over
/// the game or a demo: three pixels in four go to palette index 0 in a fixed
/// dither — row `y` keeps only the pixels with `x & 3 == (y & 1) << 1`. The
/// pattern is laid on the 320x200 virtual screen, scaled like the rest of the
/// 2-D layer (each virtual pixel a `scale x scale` block).
pub fn fade_screen(image: &mut Image, palette: &[[u8; 3]; 256]) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let scale = (image.w as f32 / MENU_VIRT_W).min(image.h as f32 / MENU_VIRT_H);
    let inv = if scale.is_finite() && scale > 0.0 { 1.0 / scale } else { 1.0 };
    let black = palette[0];
    // Virtual column of each framebuffer column, computed once.
    let vcols: Vec<usize> = (0..image.w).map(|x| (x as f32 * inv) as usize).collect();
    for y in 0..image.h {
        let vy = (y as f32 * inv) as usize;
        let t = (vy & 1) << 1;
        let row = &mut image.rgb[y * image.w..(y + 1) * image.w];
        for (px, &vx) in row.iter_mut().zip(vcols.iter()) {
            if vx & 3 != t {
                *px = black;
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

    // M_Draw: the game/demo underneath fades first (Draw_FadeScreen). (The
    // C's other branch, the console background under a forced-up console,
    // can't occur: this port's menu and console never share the screen.)
    fade_screen(image, palette);

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
            m_print(image, cc, OPTIONS_LABEL_X, ry, label, scale, ox, oy, palette);
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
            // M_DrawCheckbox: M_Print (x, y, "on" / "off").
            m_print(image, cc, OPTIONS_WIDGET_X, ry, checkbox_text(on), scale, ox, oy, palette);
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
            m_print(image, cc, 16.0, ry, row, scale, ox, oy, palette);
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
    // Every string on this screen is M_Print (bronze).
    if menu.bind_grabbing() {
        m_print(
            image, cc, 12.0, 32.0, "Press a key or button for this action", scale, ox, oy,
            palette,
        );
    } else {
        m_print(
            image, cc, 18.0, 32.0, "Enter to change, backspace to clear", scale, ox, oy, palette,
        );
    }
    for (i, (_, label)) in BINDNAMES.iter().enumerate() {
        let y = 48.0 + 8.0 * i as f32;
        m_print(image, cc, 16.0, y, label, scale, ox, oy, palette);
        let keys = menu.find_keys_for_command(i);
        match keys[0] {
            None => m_print(image, cc, 140.0, y, "???", scale, ox, oy, palette),
            Some(k0) => {
                let name = keynum_to_string(k0);
                m_print(image, cc, 140.0, y, &name, scale, ox, oy, palette);
                if let Some(k1) = keys[1] {
                    // M_Print (140 + x + 8, y, "or"); M_Print (140 + x + 32, ...).
                    let x = name.len() as f32 * 8.0;
                    m_print(image, cc, 140.0 + x + 8.0, y, "or", scale, ox, oy, palette);
                    m_print(
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
/// from y=36 (`WIDTHxHEIGHT`, bronze; the current mode white, as the C marks
/// it), the flashing cursor on the highlighted row, and hint lines.
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
    // VID_MenuDraw prints every mode with M_Print (bronze) except the current
    // one, which it prints with M_PrintWhite.
    let current = menu.resolution();
    for (i, &(w, h)) in RESOLUTION_PRESETS.iter().enumerate() {
        let y = 36.0 + 8.0 * i as f32;
        let row = format!("{w}x{h}");
        if (w, h) == current {
            draw_string_scaled(image, cc, 16.0, y, &row, scale, ox, oy, palette);
        } else {
            m_print(image, cc, 16.0, y, &row, scale, ox, oy, palette);
        }
    }
    let cy = 36.0 + menu.cursor as f32 * 8.0;
    draw_char_scaled(image, cc, 8.0, cy, cursor_glyph, scale, ox, oy, palette);
    // The C's bottom hints ("Press enter to set mode" / "Esc to exit"), at this
    // single column's foot.
    let hints_y = 36.0 + RESOLUTION_PRESETS.len() as f32 * 8.0 + 16.0;
    m_print(image, cc, 9.0 * 8.0, hints_y, "Press Enter to set mode", scale, ox, oy, palette);
    m_print(image, cc, 15.0 * 8.0, hints_y + 16.0, "Esc to exit", scale, ox, oy, palette);
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
    use crate::render::fixtures::{
        ramp_palette, solid_conchars, solid_pic, synthetic_liquid_pixels, synthetic_sky_pixels,
        test_backtile,
    };
    use crate::wad::{Qpic, Wad2, CMP_NONE, LUMPINFO_SIZE, NAME_LEN, TYP_QPIC, WADINFO_SIZE};

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
        let mut faded = Image::new(320, 200, [9, 9, 9]);
        fade_screen(&mut faded, &pal);
        let mut m = Menu::new();
        m.open();
        // All pics absent: only M_Draw's Draw_FadeScreen shows, and no panic.
        let pics = MenuPics::default();
        draw_menu(&mut img, &m, &pics, None, 0.3, 0.0, &pal);
        assert_eq!(img.rgb, faded.rgb, "an all-empty MenuPics draws only the fade");

        // A hidden menu never draws (not even the fade).
        m.close();
        let before = img.rgb.clone();
        let solid = solid_pic(64, 16, 7);
        let pics2 = MenuPics { mainmenu: Some(solid), ..Default::default() };
        draw_menu(&mut img, &m, &pics2, None, 0.3, 0.0, &pal);
        assert_eq!(img.rgb, before, "a hidden menu must not draw");
    }

    #[test]
    fn fade_screen_blackens_three_pixels_in_four_like_draw_fadescreen() {
        // Draw_FadeScreen: row y keeps only x & 3 == (y & 1) << 1; the rest go
        // to palette index 0.
        let mut pal = ramp_palette();
        pal[0] = [1, 2, 3];
        let keep = [200u8, 100, 50];
        let mut img = Image::new(320, 200, keep);
        fade_screen(&mut img, &pal);
        for y in 0..200 {
            for x in 0..320 {
                let want = if x & 3 == (y & 1) << 1 { keep } else { pal[0] };
                assert_eq!(img.rgb[y * 320 + x], want, "({x},{y})");
            }
        }
        // Scaled 2-D layer: at 640x400 each virtual pixel is a 2x2 block.
        let mut big = Image::new(640, 400, keep);
        fade_screen(&mut big, &pal);
        for &(x, y) in &[(0, 0), (1, 1), (4, 2), (5, 3), (2, 0), (0, 2), (639, 399)] {
            let (vx, vy) = (x / 2, y / 2);
            let want = if vx & 3 == (vy & 1) << 1 { keep } else { pal[0] };
            assert_eq!(big.rgb[y * 640 + x], want, "({x},{y})");
        }
        let kept = big.rgb.iter().filter(|&&p| p == keep).count();
        assert_eq!(kept, 640 * 400 / 4, "a quarter of the screen survives");
        fade_screen(&mut Image::new(0, 0, keep), &pal);
    }

    #[test]
    fn options_labels_are_m_print_bronze_and_the_current_video_mode_white() {
        // M_Print draws cell c + 128 (the conchars' bronze half); M_PrintWhite
        // the plain cell. A conchars whose bronze 'S' (211) is index 5 and
        // plain 'S' (83) index 6 tells them apart on the Options "Screen size"
        // label and on the Video list.
        let pal = ramp_palette();
        let mut data = vec![0u8; 128 * 128];
        let mut fill = |cell: usize, idx: u8| {
            let (cx, cy) = ((cell % 16) * 8, (cell / 16) * 8);
            for y in 0..8 {
                for x in 0..8 {
                    data[(cy + y) * 128 + cx + x] = idx;
                }
            }
        };
        for c in 32..127usize {
            fill(c, 6); // white half
            fill(c + 128, 5); // bronze half
        }
        let conchars = crate::wad::Qpic { width: 128, height: 128, data };
        let mut m = Menu::new();
        m.open();
        m.cursor = 2;
        m.select(); // -> Options
        let mut img = Image::new(320, 200, [0, 0, 0]);
        draw_menu(&mut img, &m, &MenuPics::default(), Some(&conchars), 0.0, 0.0, &pal);
        // "           Screen size" at (16, 56): the 'S' is the 12th character.
        let s_px = (56 + 3) * 320 + 16 + 11 * 8 + 3;
        assert_eq!(img.rgb[s_px], pal[5], "Options labels are M_Print (bronze)");
        // Video Options: the current mode white, the others bronze.
        m.sync_resolution(640, 400);
        m.cursor = ROW_VIDEO;
        m.select();
        let mut img = Image::new(320, 200, [0, 0, 0]);
        draw_menu(&mut img, &m, &MenuPics::default(), Some(&conchars), 0.0, 0.0, &pal);
        let row_px = |row: usize| (36 + row * 8 + 3) * 320 + 16 + 3;
        assert_eq!(img.rgb[row_px(2)], pal[6], "640x400 (current) is M_PrintWhite");
        assert_eq!(img.rgb[row_px(0)], pal[5], "320x200 is M_Print");
        assert_eq!(img.rgb[row_px(6)], pal[5], "1280x800 is M_Print");
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

        // Reset to defaults: exec default.cfg restores what that file sets
        // (viewsize/gamma/volume/sensitivity + the binds) and nothing else.
        m.open();
        m.cursor = 2;
        m.select(); // -> Options
        m.cursor = ROW_MOUSESPEED;
        m.adjust(1);
        m.adjust(1);
        m.cursor = ROW_BRIGHTNESS;
        m.adjust(1);
        m.cursor = ROW_SNDVOLUME;
        m.adjust(-1);
        m.cursor = ROW_CDVOLUME;
        m.adjust(-1);
        m.cursor = ROW_ALWAYSRUN;
        m.adjust(1); // toggles OFF (Always Run defaults on in this port)
        m.cursor = ROW_INVERTMOUSE;
        m.adjust(1);
        m.cursor = ROW_LOOKSPRING;
        m.adjust(1);
        m.cursor = ROW_LOOKSTRAFE;
        m.adjust(1);
        assert!(m.sensitivity() != SENS_DEFAULT && !m.always_run());
        m.cursor = ROW_DEFAULTS;
        assert_eq!(m.select(), MenuAction::ResetDefaults);
        assert!((m.sensitivity() - SENS_DEFAULT).abs() < 1e-6, "sensitivity 3");
        assert!((m.gamma() - GAMMA_DEFAULT).abs() < 1e-6, "gamma 1.0");
        assert!((m.volume() - VOLUME_DEFAULT).abs() < 1e-6, "volume 0.7");
        // default.cfg never touches these: they keep the player's values.
        assert!((m.bgm_volume() - 0.9).abs() < 1e-6, "bgmvolume kept");
        assert!(!m.always_run(), "cl_forwardspeed kept (Always Run stays off)");
        assert!(m.invert_mouse() && m.lookspring() && m.lookstrafe(), "m_pitch/lookspring/lookstrafe kept");

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
