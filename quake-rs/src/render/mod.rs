//! The software renderer: id's WinQuake 3-D view, driven by the parsed BSP data.
//!
//! The world and the brush entities go through a port of id's edge-sorted span
//! renderer (`edge`: `r_bsp.c`, `r_draw.c`, `r_edge.c`, `d_edge.c`) — the BSP
//! walked front to back into one edge list, spans emitted per scanline for the
//! nearest surface, each pixel drawn once over the surface cache with
//! `D_DrawSpans16`, and a 16-bit 1/z buffer left for the entities: alias
//! models (`D_PolysetDraw`), sprites and particles. [`render_bsp`] is a
//! flat-shaded triangle debug view with no textures or lightmaps.
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
//!
//! ## Layout
//!
//! This file keeps `r_main.c`'s share: [`Image`], [`Camera`], the flat
//! [`render_bsp`], and the [`Scene`] a [`Renderer`] draws (`R_RenderView`,
//! `R_NewMap`). The rest
//! follows id's files: `view` (view.c), `edge` (r_bsp.c, r_draw.c, r_edge.c,
//! d_edge.c), `world` (the brush entities handed to it), `raster` (the span
//! routines, d_scan.c), `light` (r_light.c, `R_BuildLightMap`), `surf` (r_surf.c,
//! d_surf.c), `warp` (d_scan.c's turbulence), `sky` (r_sky.c, d_sky.c), `vis`
//! (`Mod_PointInLeaf`), `alias` (r_alias.c, r_aclip.c), `polyse`
//! (d_polyse.c), `sprite` (r_sprite.c), `part` (r_part.c), `stats` (the
//! profiler). The 2-D layer is beside it: [`crate::draw`], [`crate::screen`],
//! [`crate::sbar`], [`crate::menu`], [`crate::keys`], [`crate::console`].

use crate::bsp::Bsp;
use crate::math::{cross, dot, normalize, sub, Vec3};
use alias::{prepare_alias_model, prepare_viewmodel, AliasDraw};
use raster::{hash_color, raster_triangle, Projected};
use sprite::draw_sprites;
use warp::TurbTable;

mod view;
mod band;
mod edge;
mod raster;
mod light;
mod surf;
mod warp;
mod sky;
mod vis;
mod world;
mod alias;
mod polyse;
mod sprite;
mod part;
mod stats;
mod video;
#[cfg(test)]
pub(crate) mod fixtures;

// The 2-D layer's names quake-wasm and quaketool reach as `render::X`.
pub use crate::console::{draw_console, draw_notify, Console};
pub use crate::draw::conchars_pic;
pub use crate::menu::{
    draw_menu, draw_menu_over_console, Extra, Extras, Menu, MenuAction, MenuPics, MenuScreen, MenuSound, WebExtra,
    BIND_ATTACK, BIND_BACK, BIND_CENTERVIEW, BIND_CHANGEWEAPON, BIND_FORWARD, BIND_JUMP,
    BIND_LEFT, BIND_LOOKDOWN, BIND_LOOKUP, BIND_MOVEDOWN, BIND_MOVELEFT, BIND_MOVERIGHT,
    BIND_MOVEUP, BIND_RIGHT, BIND_SIZEDOWN, BIND_SIZEUP, BIND_SPEED, BIND_STRAFE, NEW_GAME_MAP,
    NUM_HELP_PAGES, RESOLUTION_PRESETS, WEB_EXTRAS,
};
pub use crate::sbar::{
    draw_finale_overlay, draw_hud_into, draw_intermission_overlay, Hud, IntermissionStats,
};
pub use crate::screen::{
    calc_refdef, compose_view, draw_centerprint, draw_fps, draw_pause, screen_with_backtile, vid_aspect, ViewRect,
    SB_LINES_FULL, VIEWSIZE_DEFAULT,
};
// The renderer's public API (its files are private).
pub use alias::{ModelInstance, Viewmodel};
pub use light::{LIGHTSTYLES, NEUTRAL_LIGHTSTYLE_SCALES};
pub use part::draw_particles;
pub use sprite::SpriteInstance;
pub use surf::MipCvars;
pub use stats::RenderStats;
pub use view::{
    build_gamma_table, content_cshift, cshift_ramps, pack_rgba, powerup_cshift, view_bob, viewmodel_angles,
    viewmodel_fudge, viewmodel_origin_ofs,
};
pub use vis::point_in_leaf;
pub use video::{FovMode, VideoCvars, HIRES_MAXHEIGHT, HIRES_MAXWIDTH, MAXHEIGHT, MAXWIDTH};
pub use world::{BModelInstance, ExternalBModel};
pub(crate) use band::map_rows;

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

    /// A `w * h` image on a spare frame buffer whose old pixels are LEFT IN
    /// PLACE (only growth is filled, black): for a caller that writes every
    /// pixel, so the clear would be wasted.
    pub(crate) fn reused_uncleared(w: usize, h: usize) -> Image {
        let mut rgb = take_spare_rgb();
        rgb.resize(w.saturating_mul(h), [0, 0, 0]);
        Image { w, h, rgb }
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

    /// Copy `view` into this image with its top-left corner at `(x, y)`, as
    /// far as it fits, in runs of rows on up to `threads` threads.
    pub(crate) fn blit(&mut self, view: &Image, x: usize, y: usize, threads: usize) {
        let (sw, sh) = (self.w, self.h);
        let (x0, y0) = (x.min(sw), y.min(sh));
        let (cw, ch) = (view.w.min(sw - x0), view.h.min(sh - y0));
        if cw == 0 || ch == 0 || self.rgb.len() < sw * sh || view.rgb.len() < view.w * view.h {
            return;
        }
        let vw = view.w;
        map_rows(threads, ch, &mut self.rgb[y0 * sw..(y0 + ch) * sw], sw, &view.rgb[..ch * vw], vw, |dst, src| {
            for (d, s) in dst.chunks_mut(sw).zip(src.chunks(vw)) {
                d[x0..x0 + cw].copy_from_slice(&s[..cw]);
            }
        });
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
// Frame buffers kept across frames
// ---------------------------------------------------------------------------
//
// Quake allocates its frame buffers once per video mode — `vid.buffer`, the
// z-buffer `d_pzbuffer`, `r_warpbuffer` — and draws into them every frame. The
// z-buffer is the [`Renderer`]'s. The port's frame is an `Image` returned by
// value, so the rest is a small per-thread pool: the host hands a presented
// frame back ([`recycle_image`]) and the next frame's view
// ([`Renderer::render`]), composed screen ([`compose_view`]) and warp
// snapshot ([`Renderer::warp`]) reuse the allocations. It is the host's frame
// allocator, not renderer state: every reuse writes every pixel
// ([`Image::reused_uncleared`]), so nothing drawn depends on it, and only the
// thread that runs the frame loop takes from it.

/// Spare frame buffers kept: one frame's view, composed screen and warp
/// snapshot.
const SPARE_FRAMES: usize = 3;

thread_local! {
    /// Pixel buffers of frames handed back by [`recycle_image`].
    static SPARE_RGB: std::cell::RefCell<Vec<Vec<[u8; 3]>>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Hand a finished frame's pixel buffer back so the next frame reuses it
/// instead of allocating (the host calls this once the frame is presented).
/// Nothing drawn depends on whether it is called.
pub fn recycle_image(image: Image) {
    recycle_rgb(image.rgb);
}

/// [`recycle_image`] for a bare pixel buffer (the warp's snapshot).
pub(crate) fn recycle_rgb(rgb: Vec<[u8; 3]>) {
    if rgb.capacity() == 0 {
        return;
    }
    SPARE_RGB.with(|s| {
        let mut s = s.borrow_mut();
        if s.len() < SPARE_FRAMES {
            s.push(rgb);
        }
    });
}

/// A spare pixel buffer (old contents and all), or an empty one.
pub(crate) fn take_spare_rgb() -> Vec<[u8; 3]> {
    SPARE_RGB.with(|s| s.borrow_mut().pop()).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Camera
// ---------------------------------------------------------------------------

/// A pinhole camera positioned in Quake world space. `yaw` rotates about `+Z`
/// (0 = facing `+X`, increasing toward `+Y`); `pitch` tilts the forward vector
/// up/down. Both are in degrees, as is the horizontal field of view `fov_deg`
/// (`scr_fov`: with [`FovMode::HorPlus`] the view's own is wider).
#[derive(Clone, Copy, Debug, PartialEq)]
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
// The view: what R_ViewChanged derives beyond the camera
// ---------------------------------------------------------------------------

/// How a frame is drawn beyond what the [`Camera`] says: the refdef state
/// `R_ViewChanged` (`r_main.c`) is handed besides the field of view, the cvars
/// the renderer reads each frame, and the port's opt-in extras. [`Default`] is
/// id's `vid_null.c` view (square pixels) with id's cvars.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderOptions {
    /// `vid.aspect`, which `R_ViewChanged` takes as `pixelAspect`: the width
    /// of a displayed pixel over its height. 1.0 is square pixels. id's DOS and
    /// Windows drivers set `(vid.height / vid.width) * (320/240)` — the mode is
    /// shown on a 4:3 monitor, so 320x200 has pixels 1.2x taller than wide and
    /// `vid.aspect` 0.8333 ([`crate::screen::vid_aspect`]). It scales every
    /// vertical projection (`yscale = xscale * pixelAspect`): the world and
    /// brush models, alias models and the gun, sprites, particles and the
    /// frustum. The sky does not use it (`D_Sky_uv_To_st` maps screen pixels).
    pub pixel_aspect: f32,
    /// Where the view sits on the screen: `r_refdef.vrect`'s corner in the
    /// `vid.width x vid.height` framebuffer. `None` (the default): the view is
    /// the whole screen. The sky needs it: `D_Sky_uv_To_st` centres the sky on
    /// the SCREEN (`vid.width>>1`, `vid.height>>1`), not on the view, so a view
    /// above the status bar or inside a border (viewsize below 120) sees the
    /// sky off its own centre — 24 rows at 320x200 and the default viewsize 100.
    pub screen: Option<ScreenPlace>,
    /// EXTRA, not id (default off): exact perspective at every pixel of the
    /// surface-cached walls and the liquids. id's x86 renderer, what 1996
    /// players saw, is exact only every 16 pixels and affine in between
    /// (`D_DrawSpans16`, `Turbulent8`); that is the default. The browser's
    /// `wasm_exactpersp 1` sets this.
    pub exact_perspective: bool,
    /// The port's video cvars: Hor+ and views past id's largest
    /// ([`VideoCvars`]; Classic by default).
    pub video: VideoCvars,
    /// `d_mipscale` / `d_mipcap` (`D_SetupFrame` reads them every frame; id's
    /// by default).
    pub mip: MipCvars,
}

/// A view's place on the screen ([`RenderOptions::screen`]): its top-left
/// corner `(x, y)` in a `vid_w x vid_h` framebuffer — `r_refdef.vrect.x/y` and
/// `vid.width/height`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenPlace {
    pub x: usize,
    pub y: usize,
    pub vid_w: usize,
    pub vid_h: usize,
}

impl Default for RenderOptions {
    fn default() -> RenderOptions {
        RenderOptions {
            pixel_aspect: 1.0,
            screen: None,
            exact_perspective: false,
            video: VideoCvars::CLASSIC,
            mip: MipCvars::DEFAULT,
        }
    }
}

impl RenderOptions {
    /// `D_Sky_uv_To_st`'s screen centre in a `w x h` view's own pixels:
    /// `(vid.width>>1) - vrect.x`, `(vid.height>>1) - vrect.y`.
    fn sky_centre(&self, w: usize, h: usize) -> (i32, i32) {
        match self.screen {
            Some(p) => ((p.vid_w as i32 >> 1) - p.x as i32, (p.vid_h as i32 >> 1) - p.y as i32),
            None => (w as i32 >> 1, h as i32 >> 1),
        }
    }

    /// The span routine for textured brush surfaces.
    fn persp(&self) -> raster::Persp {
        if self.exact_perspective {
            raster::Persp::Exact
        } else {
            raster::Persp::Spans16
        }
    }

    /// [`RenderOptions::pixel_aspect`], with a non-finite or non-positive value
    /// read as square pixels.
    pub(crate) fn aspect(&self) -> f32 {
        let a = self.pixel_aspect;
        if a.is_finite() && a > 0.0 {
            a
        } else {
            1.0
        }
    }
}

/// `R_ViewChanged`'s projection of a `w x h` view: a view-space point
/// `(vx, vy, vz)` (along `vright`, `vup`, `vpn`) lands at
/// `x = cx + xscale*vx/vz`, `y = cy - yscale*vy/vz`.
///
/// `xscale = vrect.width / horizontalFieldOfView` = `(w/2) / tan(fov_x/2)` and
/// `yscale = xscale * pixelAspect`; the vertical field of view follows from
/// them (the software renderer never uses `fov_y`). The centre is `w/2, h/2`
/// because pixel `(px, py)` has its centre at `(px + 0.5, py + 0.5)` here: id's
/// `xcenter = w/2 - 0.5` with centres on the integers.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Projection {
    pub(crate) cx: f32,
    pub(crate) cy: f32,
    pub(crate) xscale: f32,
    pub(crate) yscale: f32,
}

impl Projection {
    pub(crate) fn new(cam: &Camera, w: usize, h: usize, pixel_aspect: f32) -> Projection {
        let cx = w as f32 / 2.0;
        let cy = h as f32 / 2.0;
        let tan_half = (cam.fov_deg as f64 * 0.5).to_radians().tan();
        // A degenerate fov falls back to ~90 degrees (xscale = cx).
        let xscale = if tan_half.abs() < 1e-6 { cx } else { (cx as f64 / tan_half) as f32 };
        Projection { cx, cy, xscale, yscale: xscale * pixel_aspect }
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

// ---------------------------------------------------------------------------
// The scene and the renderer (R_RenderView, R_NewMap)
// ---------------------------------------------------------------------------

/// A palette: 256 RGB colours (`gfx/palette.lmp`).
pub type Palette = [[u8; 3]; 256];

/// What one frame of the 3-D view shows: id's `refdef_t` (the view's size, its
/// camera and `cl.time`) with the lists `R_RenderView` walks — the world, the
/// brush entities, the alias models, the sprites, the particles and the gun —
/// and the light the frame is lit by.
///
/// [`Scene::new`] is the world alone at time 0, lit by its static lightmaps;
/// the rest is filled in with struct update syntax:
///
/// ```
/// # use quake_rs::render::{demo_room, Camera, Renderer, Scene};
/// let world = demo_room();
/// let palette = [[128u8; 3]; 256];
/// let camera = Camera::looking_at([0.0, -200.0, 0.0], [0.0, 0.0, 0.0], 90.0);
/// let scene = Scene { time: 1.5, ..Scene::new(&world, camera, 320, 200, &palette) };
/// let view = Renderer::new().render(&scene);
/// assert_eq!((view.w, view.h), (320, 200));
/// ```
#[derive(Clone, Copy)]
pub struct Scene<'a> {
    /// The world model (`cl.worldmodel`). Its faces are what the
    /// [`Renderer`]'s per-map state is indexed by ([`Renderer::begin_map`]).
    pub world: &'a Bsp,
    /// The eye, and `scr_fov` as [`Camera::fov_deg`]: the view's own field of
    /// view follows from it and the options' [`VideoCvars`] ([`FovMode::fov_x`]).
    pub camera: Camera,
    /// The view's size in pixels (`r_refdef.vrect`'s), at most the cvars'
    /// [`VideoCvars::max_view_size`]: a larger one is clamped, and the image
    /// [`Renderer::render`] returns says what was drawn.
    pub width: usize,
    pub height: usize,
    pub palette: &'a Palette,
    /// Quake's `gfx/colormap.lmp`: `64 * 256` bytes, 64 light rows of 256
    /// palette indices, row 0 brightest. With it a wall pixel is shaded as the
    /// software renderer shades it: the lightmap picks a row
    /// (`R_BuildLightMap`), `colormap[row*256 + texel]` is the palette index
    /// drawn — Quake's non-linear darkening, never brighter than the texel.
    /// Liquids and sky are fullbright, the raw texel. `None` (or one shorter
    /// than `64*256`) keeps the port's older linear `palette[texel] *
    /// brightness`, which the synthetic tests use.
    pub colormap: Option<&'a [u8]>,
    /// `cl.time` in seconds: the liquid turb (`Turbulent8`), the sky's
    /// two-layer scroll, animated wall textures (`R_TextureAnimation`) and
    /// alias frame and skin groups all run on it. At 0 they show their first
    /// frame.
    pub time: f32,
    /// The brightness of each light style (`d_lightstylevalue` / 256, `1.0`
    /// normal), from [`crate::server::Server::lightstyle_scales`]
    /// (`R_AnimateLight`): a face's lightmap is the sum of its up to four
    /// styles' luxel blocks, each scaled by its value, before dynamic light is
    /// added. [`NEUTRAL_LIGHTSTYLE_SCALES`] is the static style-0 lightmap.
    pub light_styles: &'a [f32; LIGHTSTYLES],
    /// The live dynamic lights (explosions, muzzle flashes, `EF_*` effects),
    /// folded into every lightmapped face of the world and the inline brush
    /// models they reach (`R_PushDlights`, `R_AddDynamicLights`), and into the
    /// alias models' light. Empty leaves every face its static lightmap.
    pub dlights: &'a [crate::dlight::DynamicLight],
    /// The inline brush entities (doors, lifts, buttons: the world's own
    /// `*N` models), put into the world's edge list at their origins
    /// (`R_DrawBEntitiesOnList`).
    pub bmodels: &'a [BModelInstance],
    /// The standalone `b_*.bsp` item boxes (`misc_explobox`, the ammo and
    /// health boxes): each borrows its own parsed [`Bsp`] and joins the edge
    /// list after the inline models, as id's instanced brush models do. They
    /// take their static lightmaps (id never marks them for dynamic light).
    pub external: &'a [ExternalBModel<'a>],
    /// The alias models (`cl_visedicts`' `mod_alias` entities), drawn after
    /// the world against its z-buffer (`R_DrawEntitiesOnList`).
    pub models: &'a [ModelInstance<'a>],
    /// The sprite entities (`mod_sprite`: the explosion flash, bubbles),
    /// z-tested after the alias models.
    pub sprites: &'a [SpriteInstance<'a>],
    /// The particles as `(world position, palette index)`, drawn after the
    /// sprites against the same z-buffer (`R_DrawParticles`). id draws them
    /// after the gun; with the gun's tripled 1/z the order only matters on
    /// exact ties.
    pub particles: &'a [(Vec3, u8)],
    /// The first-person weapon, drawn last (`R_DrawViewModel`), or `None`.
    pub viewmodel: Option<Viewmodel<'a>>,
    /// The pixel aspect, the view's place on the screen, the cvars and the
    /// renderer extras.
    pub options: RenderOptions,
}

impl<'a> Scene<'a> {
    /// The world alone, seen by `camera` in a `width x height` view, at time
    /// 0 with neutral light styles, no colormap and no entities.
    pub fn new(world: &'a Bsp, camera: Camera, width: usize, height: usize, palette: &'a Palette) -> Scene<'a> {
        Scene {
            world,
            camera,
            width,
            height,
            palette,
            colormap: None,
            time: 0.0,
            light_styles: &NEUTRAL_LIGHTSTYLE_SCALES,
            dlights: &[],
            bmodels: &[],
            external: &[],
            models: &[],
            sprites: &[],
            particles: &[],
            viewmodel: None,
            options: RenderOptions::default(),
        }
    }
}

/// A frame as its passes see it: the [`Scene`] with its size clamped to the
/// cvars' largest view and its camera's field of view the view's own
/// (`R_ViewChanged`'s `fov_x`: Hor+ widens it).
struct Frame<'s, 'a> {
    scene: &'s Scene<'a>,
    /// The view's camera: the scene's, with the view's field of view.
    cam: Camera,
    w: usize,
    h: usize,
    /// The liquids' `sintable` (`R_InitTurb`).
    turb: TurbTable,
}

impl<'s, 'a> Frame<'s, 'a> {
    /// `scene` in a `w x h` view (already clamped to the cvars' largest).
    fn new(scene: &'s Scene<'a>, w: usize, h: usize) -> Frame<'s, 'a> {
        let opts = &scene.options;
        let (screen_w, screen_h) = opts.screen.map_or((w, h), |s| (s.vid_w, s.vid_h));
        let fov_x = opts.video.fov_mode.fov_x(scene.camera.fov_deg, screen_w, screen_h, opts.aspect());
        let cam = Camera { fov_deg: fov_x, ..scene.camera };
        Frame { scene, cam, w, h, turb: TurbTable::new() }
    }

    /// `scr_fov`, the cvar: what id tests on the cvar itself (no gun over 90,
    /// the sky's scale), where the passes project with [`Frame::cam`]'s.
    fn scr_fov(&self) -> f32 {
        self.scene.camera.fov_deg
    }
}

/// id's software renderer: everything it keeps between frames, owned.
///
/// id keeps it in globals and in the model: the edge cache in `medge_t`, the
/// leaf keys and visframes (`r_bsp.c`, `r_edge.c`), the surface cache
/// (`d_surf.c`), `d_pzbuffer` and the warp tables. Here one value owns it all,
/// so a frame's passes are handed exactly the state they use and a second
/// renderer (or a second thread) can never see another's:
/// [`Renderer::begin_map`] is `R_NewMap`, [`Renderer::render`] is
/// `R_RenderView`. What a frame is drawn with — the cvars included — is the
/// [`Scene`]'s, handed in each frame.
///
/// The per-map state is indexed by the world's face, edge, node and leaf
/// numbers, so it belongs to one world: call [`Renderer::begin_map`] whenever
/// the world changes (a new map, a changelevel). A scene whose world has a
/// different shape (a different count of faces, edges, nodes, leaves or
/// lighting) than the one begun starts a new map itself, so a forgotten
/// `begin_map` costs a cold cache, never a wrong pixel.
pub struct Renderer {
    /// The shape of the world [`Renderer::begin_map`] was called for.
    map: Option<MapShape>,
    /// The edge renderer's state (`r_bsp.c`, `r_draw.c`, `r_edge.c`).
    edge: edge::EdgeState,
    /// The world's per-face caches, the surface cache among them.
    surfaces: surf::SurfaceCaches,
    /// id's z-buffer, `d_pzbuffer`: the 16-bit 1/z of every pixel of the view,
    /// `(1/z * 0x8000 * 0x10000) >> 16` (larger is nearer). Never cleared:
    /// every frame's world spans write all of it (`D_DrawZSpans`), and the
    /// entities test and write it.
    zbuf: Vec<i16>,
    /// `D_WarpScreen`'s tables, kept across underwater frames.
    warp: warp::WarpTables,
    prof: stats::Profiler,
    /// How many threads draw a frame's bands.
    workers: band::Workers,
}

/// A world's identity for [`Renderer`]'s per-map state: the sizes of what
/// that state is indexed by. Not the world's address — a world is begun
/// explicitly ([`Renderer::begin_map`]); this only catches a scene of a
/// different world handed to a renderer that was not told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MapShape {
    faces: usize,
    edges: usize,
    nodes: usize,
    leafs: usize,
    lighting: usize,
}

impl MapShape {
    fn of(world: &Bsp) -> MapShape {
        MapShape {
            faces: world.faces.len(),
            edges: world.edges.len(),
            nodes: world.nodes.len(),
            leafs: world.leafs.len(),
            lighting: world.lighting.len(),
        }
    }
}

impl Default for Renderer {
    fn default() -> Renderer {
        Renderer::new()
    }
}

impl Renderer {
    /// A renderer with no map begun and the profiler off.
    pub fn new() -> Renderer {
        Renderer {
            map: None,
            edge: edge::EdgeState::new(),
            surfaces: surf::SurfaceCaches::default(),
            zbuf: Vec::new(),
            warp: warp::WarpTables::default(),
            prof: stats::Profiler::default(),
            workers: band::Workers::default(),
        }
    }

    /// `R_NewMap` (`r_misc.c`): forget everything kept for the last world and
    /// size the per-map state for `world` — the edge cache, the visframes and
    /// parents of its nodes and leaves, and the per-face caches, all at zero
    /// as `Mod_LoadBrushModel` leaves them.
    pub fn begin_map(&mut self, world: &Bsp) {
        self.map = Some(MapShape::of(world));
        self.edge.begin_map(world);
        self.surfaces.begin_map(world.faces.len());
    }

    /// Turn the profiler on with its counters at zero: the frames drawn from
    /// now on add into [`RenderStats`] until [`Renderer::stats_end`].
    pub fn stats_begin(&mut self) {
        self.prof.begin();
    }

    /// What the profiler counted since [`Renderer::stats_begin`]; it is off
    /// again.
    pub fn stats_end(&mut self) -> RenderStats {
        self.prof.end()
    }

    /// Install (or clear) the profiler's clock: a monotonic milliseconds
    /// source, for a target without `std::time::Instant` (the browser's
    /// `performance.now()`). Only read while profiling.
    pub fn set_stats_clock(&mut self, clock: Option<fn() -> f64>) {
        self.prof.set_clock(clock);
    }

    /// The bytes the lit-surface cache holds now (every baked block of every
    /// face at every mip level), and the number of blocks: the port's
    /// counterpart of id's fixed `D_SurfaceCacheForRes` pool, for measurement.
    #[must_use]
    pub fn surface_cache_usage(&self) -> (usize, usize) {
        self.surfaces.usage()
    }

    /// Draw `scene` as `R_RenderView` does: `R_EdgeDrawing` — the world and
    /// every brush entity through one edge list (`edge.rs`), each pixel of the
    /// view drawn once and its 1/z written into the never-cleared 16-bit
    /// `d_pzbuffer` — then the alias models, the sprites and the particles,
    /// each testing and writing that z-buffer, and last the gun
    /// (`R_DrawViewModel`, its 1/z tripled so only a wall against the eye
    /// covers it). Neither the image nor the z-buffer is cleared: the spans
    /// cover the view.
    ///
    /// The view is at most [`VideoCvars::max_view_size`] — id's [`MAXWIDTH`] x
    /// [`MAXHEIGHT`] in Classic: a larger scene is clamped, and the returned
    /// image's `w`/`h` say what was drawn. With [`FovMode::HorPlus`] every
    /// pass draws with the wider field of view it gives this screen, while
    /// the scene's `camera.fov_deg` stays `scr_fov` for what id tests on the
    /// cvar itself (no gun over 90, the sky's scale).
    pub fn render(&mut self, scene: &Scene) -> Image {
        // No mode is larger than the cvars allow (id's, or 8K with hires).
        let (w, h) = scene.options.video.clamp_to_max(scene.width, scene.height);
        // The frame's pixels, on a spare buffer (see [`recycle_image`]).
        let mut image = Image::reused_uncleared(w, h);
        self.draw(scene, w, h, &mut image.rgb, w, 0);
        image
    }

    /// [`Renderer::render`] straight into `screen`, at the view's place on it
    /// (`scene.options.screen`'s corner; the whole of `screen` without one),
    /// as id's `R_RenderView` draws into `vid.buffer`: no view image, no copy.
    /// The view's rectangle is written whole; the rest of the screen is left
    /// as it was. A view that does not fit in `screen` (never the client's)
    /// is drawn apart and copied in as far as it fits.
    pub fn render_into(&mut self, scene: &Scene, screen: &mut Image) {
        let (w, h) = scene.options.video.clamp_to_max(scene.width, scene.height);
        let (x, y) = scene.options.screen.map_or((0, 0), |p| (p.x, p.y));
        let sw = screen.w;
        let fits = x + w <= sw && y + h <= screen.h && screen.rgb.len() >= sw.saturating_mul(screen.h);
        if fits {
            self.draw(scene, w, h, &mut screen.rgb[y * sw..(y + h) * sw], sw, x);
        } else {
            let view = self.render(scene);
            screen.blit(&view, x, y, self.threads());
            recycle_image(view);
        }
    }

    /// The frame of `scene`, `w x h` (already clamped), into `rows`: `stride`
    /// pixels a row, the view at column `x0` of each. What the whole frame
    /// decides is done first — the world's edges, spans and surfaces (the
    /// surface cache filled), the entities up to their rasterisers — and then
    /// every band of the view draws from it, on the renderer's threads.
    fn draw(&mut self, scene: &Scene, w: usize, h: usize, rows: &mut [[u8; 3]], stride: usize, x0: usize) {
        if w == 0 || h == 0 {
            return;
        }
        if self.map != Some(MapShape::of(scene.world)) {
            self.begin_map(scene.world);
        }
        self.zbuf.resize(w.saturating_mul(h), 0);
        let frame = Frame::new(scene, w, h);
        let Some(world) = self.edge.build(&frame, &mut self.surfaces, &mut self.prof) else {
            return;
        };
        let entities = Entities::prepare(&frame, &mut self.prof);
        let t = self.prof.now();
        let (edge, prof, workers) = (&self.edge, &self.prof, self.workers);
        let whole = band::Band::placed(w, rows, stride, x0, &mut self.zbuf);
        let bands = workers.run(whole, h, || prof.for_band(), |band, prof| {
            let tw = prof.now();
            let drawn = edge.draw_band(band, &frame, &world);
            if let Some(tw) = tw {
                let ns = tw.elapsed().as_nanos() as u64;
                prof.add(|s| {
                    s.world_ns += ns;
                    s.world_surf_ns += ns;
                });
            }
            prof.add(|s| s.world_pixels += drawn);
            entities.draw(band, &frame, prof);
        });
        let threads = bands.len() as u64;
        for b in &bands {
            self.prof.absorb(b);
        }
        if let Some(t) = t {
            let ns = t.elapsed().as_nanos() as u64;
            self.prof.add(|s| {
                s.bands_ns += ns;
                s.band_threads += threads;
            });
        }
    }

    /// How many threads draw a frame (1, the default: the calling thread
    /// alone). The frame is the same for any count; see `band.rs`.
    #[must_use]
    pub fn threads(&self) -> usize {
        self.workers.threads()
    }

    /// Draw the frames from now on on `threads` threads (at least 1): the
    /// calling thread and `threads - 1` more, spawned for each frame's bands
    /// (`std::thread::scope`) and joined before [`Renderer::render`] returns.
    pub fn set_threads(&mut self, threads: usize) {
        self.workers = band::Workers::new(threads);
    }

    /// The z-buffer the last frame left.
    #[cfg(test)]
    pub(crate) fn zbuf(&self) -> &[i16] {
        &self.zbuf
    }

    /// `D_WarpScreen` (`d_scan.c`), for an underwater frame: `view`, rendered
    /// at the warp rectangle ([`crate::screen::warp_vrect`]), wobbled and
    /// stretched over the rectangle `at` of `screen` at `clock`, on the
    /// renderer's threads. With the hires extra (`hires`) the wobble is scaled
    /// to the view. The view's buffer goes back to the frame pool.
    pub fn warp_into(&mut self, view: Image, screen: &mut Image, at: ViewRect, clock: f32, hires: bool) {
        let (sw, threads) = (screen.w, self.threads());
        let (x0, y0) = (at.x.min(sw), at.y.min(screen.h));
        let (w, h) = (at.w.min(sw - x0), at.h.min(screen.h - y0));
        if let Some(rows) = screen.rgb.get_mut(y0 * sw..(y0 + h) * sw) {
            let target = warp::WarpTarget { rows, stride: sw, x0, w, h };
            warp::warp_screen(&mut self.warp, &view, target, clock, hires, threads);
        }
        recycle_image(view);
    }
}

/// A frame's entities ready for the bands: `R_DrawEntitiesOnList`'s alias
/// models, `R_DrawParticles`' particles and `R_DrawViewModel`'s gun, each up
/// to its rasteriser (the sprites are cheap enough to set up per band).
struct Entities<'a> {
    models: Vec<AliasDraw<'a>>,
    particles: Vec<part::ParticleDot>,
    gun: Option<AliasDraw<'a>>,
}

impl<'a> Entities<'a> {
    /// Everything about the frame's entities that does not depend on the
    /// rows being drawn: the models' vertices, light and clipped triangles,
    /// the particles' squares.
    fn prepare(frame: &Frame<'_, 'a>, prof: &mut stats::Profiler) -> Entities<'a> {
        let scene = frame.scene;
        let models = scene.models.iter().filter_map(|inst| prepare_alias_model(frame, inst, prof)).collect();
        let opts = &scene.options;
        let proj = part::ParticleProjection::new(&frame.cam, frame.w, frame.h, opts.aspect(), opts.video.hires);
        let particles = part::project_particles(&frame.cam, &proj, scene.particles, scene.palette);
        let gun = scene.viewmodel.as_ref().and_then(|vm| prepare_viewmodel(frame, vm));
        Entities { models, particles, gun }
    }

    /// The entities' pixels in `band`'s rows, against the world's 16-bit
    /// 1/z, in id's order: the alias models, the particles, the sprites and
    /// last the gun, each testing and writing the z-buffer (id draws the
    /// particles after the gun; with the gun's tripled 1/z the order only
    /// matters on exact ties). A phase timer each while profiling.
    fn draw(&self, band: &mut band::Band, frame: &Frame, prof: &mut stats::Profiler) {
        let palette = frame.scene.palette;
        let ta = prof.now();
        for m in &self.models {
            m.draw(&mut polyse::PolyFramebuffer::new(band, palette));
        }
        if let Some(t) = ta { prof.add(|s| s.alias_ns += t.elapsed().as_nanos() as u64); }
        let tp = prof.now();
        part::draw_particle_dots(band, &self.particles);
        if let Some(t) = tp { prof.add(|s| s.particle_ns += t.elapsed().as_nanos() as u64); }
        let tsp = prof.now();
        draw_sprites(band, frame);
        if let Some(t) = tsp { prof.add(|s| s.sprite_ns += t.elapsed().as_nanos() as u64); }
        let tv = prof.now();
        if let Some(gun) = &self.gun {
            gun.draw(&mut polyse::PolyFramebuffer::new(band, palette));
        }
        if let Some(t) = tv { prof.add(|s| s.viewmodel_ns += t.elapsed().as_nanos() as u64); }
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
    //   * push 4 edges connecting them in order (wound clockwise from inside),
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
        // Wind the quad as qbsp does: clockwise seen from its front (the side
        // the normal points to), the order id's edge renderer reads leading
        // and trailing edges from (`R_EmitEdge`).
        let mut corners = corners;
        let (e1, e2) = (sub(corners[1], corners[0]), sub(corners[2], corners[1]));
        if dot(cross(e1, e2), normal) > 0.0 {
            corners.reverse();
        }
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
    use crate::render::fixtures::{render_once, synthetic_liquid_pixels, synthetic_sky_pixels};

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
        let img = render_once(&Scene::new(&bsp, cam, 160, 120, &pal));
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
    fn pixel_aspect_squashes_the_world_vertically_only() {
        // R_ViewChanged: yscale = xscale * pixelAspect. At id's 320x200-on-4:3
        // aspect (0.8333) the pillar keeps its width in pixels and its top edge
        // comes 1/6 closer to the centre row, so the 4:3 display (pixels 1.2x
        // taller than wide) shows its true shape.
        let bsp = demo_room();
        let pal = [[200u8, 200, 200]; 256];
        let cam = Camera::looking_at([-200.0, 0.0, 0.0], [0.0, 0.0, 0.0], 90.0);
        let (w, h) = (320usize, 200usize);
        let render = |pixel_aspect: f32| {
            render_once(&Scene { options: RenderOptions { pixel_aspect, ..Default::default() }, ..Scene::new(&bsp, cam, w, h, &pal) })
        };
        // The pillar's face: the colour at the centre; its columns on the centre
        // row and its top row on the centre column.
        let extent = |img: &Image| {
            let c = img.rgb[(h / 2) * w + w / 2];
            let row: Vec<usize> = (0..w).filter(|&x| img.rgb[(h / 2) * w + x] == c).collect();
            let top = (0..h).find(|&y| img.rgb[y * w + w / 2] == c).expect("pillar in view");
            (row[0], row[row.len() - 1], h / 2 - top)
        };
        let square = render(1.0);
        assert_eq!(square.rgb, render_once(&Scene::new(&bsp, cam, w, h, &pal)).rgb, "aspect 1 is the default");
        let (l1, r1, up1) = extent(&square);
        let (l2, r2, up2) = extent(&render(crate::screen::vid_aspect(w, h, 4.0 / 3.0)));
        assert_eq!((l1, r1), (l2, r2), "the width does not change");
        // 160*64/168 = 61 rows above the centre with square pixels, 51 at 0.8333.
        assert_eq!((up1, up2), (61, 51));
    }

    #[test]
    fn the_sky_is_centred_on_the_screen_not_the_view() {
        // D_Sky_uv_To_st: u - (vid.width>>1), (vid.height>>1) - v, in screen
        // pixels. A view that is the whole screen has its own centre; the 320x152
        // view above a 48-line status bar (viewsize 100) has the screen's centre
        // 24 rows below its own; the viewsize-70 view (224x140 at 48,6) is off
        // both ways.
        let at = |screen| RenderOptions { screen, ..Default::default() };
        assert_eq!(at(None).sky_centre(320, 200), (160, 100));
        assert_eq!(at(None).sky_centre(320, 152), (160, 76));
        let sbar = ScreenPlace { x: 0, y: 0, vid_w: 320, vid_h: 200 };
        assert_eq!(at(Some(sbar)).sky_centre(320, 152), (160, 100));
        let border = ScreenPlace { x: 48, y: 6, vid_w: 320, vid_h: 200 };
        assert_eq!(at(Some(border)).sky_centre(224, 140), (112, 94));
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
    fn every_thread_count_draws_the_same_frame() {
        // The bands (band.rs) give each pixel its writes in the one-thread
        // order: the world's spans (a liquid, the sky, dynamically lit walls),
        // an alias model, particles, a sprite and the gun, at a size no band
        // split divides evenly.
        let bsp = special_surface_room();
        let pal = fixtures::ramp_palette();
        let (mdl, spr) = (fixtures::tiny_mdl(), fixtures::test_sprite(12, 12, 40));
        let cam = Camera::looking_at([-200.0, -150.0, 60.0], [0.0, 0.0, 0.0], 90.0);
        let models = [ModelInstance::with_frame(&mdl, [-80.0, 0.0, 0.0], 30.0, 0, [200, 40, 40])];
        let particles: Vec<(Vec3, u8)> = (0..300)
            .map(|i| ([-150.0 + i as f32, (i % 13) as f32 * 9.0 - 60.0, (i % 7) as f32 * 9.0], (i % 250) as u8))
            .collect();
        let sprites = [SpriteInstance { sprite: &spr, origin: [-60.0, 20.0, 10.0], frame: 0 }];
        let gun = Viewmodel { mdl: &mdl, frame: 0, origin_ofs: [8.0, 0.0, -6.0], angles: [cam.pitch, cam.yaw, 0.0] };
        let dlights = [crate::dlight::DynamicLight::new([0.0; 3], 250.0, f32::MAX, 0.0, 0.0, 0)];
        let world = Scene { time: 1.3, dlights: &dlights, ..Scene::new(&bsp, cam, 211, 157, &pal) };
        let scene = Scene { models: &models, particles: &particles, sprites: &sprites, viewmodel: Some(gun), ..world };
        let one = render_once(&scene);
        let differ = one.rgb.iter().zip(&render_once(&world).rgb).filter(|(a, b)| a != b).count();
        assert!(differ > 200, "the entities show: {differ} pixels");
        for threads in [2, 3, 7, 16] {
            let mut r = Renderer::new();
            r.set_threads(threads);
            assert!(r.render(&scene).rgb == one.rgb, "{threads} threads");
            assert!(r.render(&scene).rgb == one.rgb, "{threads} threads, warm");
        }
    }

    #[test]
    fn a_view_drawn_into_its_place_on_the_screen_is_the_view_drawn_apart() {
        // render_into draws a bordered view (column 13, row 7 of a 200x150
        // screen) straight into the screen, as render draws it on its own,
        // and leaves the rest of the screen alone; a view past the screen's
        // edge is copied in as far as it fits.
        let bsp = special_surface_room();
        let pal = fixtures::ramp_palette();
        let mdl = fixtures::tiny_mdl();
        let cam = Camera::looking_at([-200.0, -150.0, 60.0], [0.0, 0.0, 0.0], 90.0);
        let models = [ModelInstance::with_frame(&mdl, [-80.0, 0.0, 0.0], 30.0, 0, [200, 40, 40])];
        let particles: Vec<(Vec3, u8)> = (0..100).map(|i| ([-120.0 + 2.0 * i as f32, 0.0, 20.0], 77)).collect();
        let (vw, vh) = (161, 117);
        let place = |x, y| RenderOptions { screen: Some(ScreenPlace { x, y, vid_w: 200, vid_h: 150 }), ..RenderOptions::default() };
        let scene = |x, y| Scene { time: 0.7, models: &models, particles: &particles, options: place(x, y), ..Scene::new(&bsp, cam, vw, vh, &pal) };
        for threads in [1, 4] {
            let mut r = Renderer::new();
            r.set_threads(threads);
            for (x, y) in [(13, 7), (100, 90)] {
                let apart = render_once(&scene(x, y));
                let mut screen = Image::new(200, 150, [1, 2, 3]);
                r.render_into(&scene(x, y), &mut screen);
                for (i, p) in screen.rgb.iter().enumerate() {
                    let (u, v) = (i % 200, i / 200);
                    let inside = (x..x + vw).contains(&u) && (y..y + vh).contains(&v);
                    let want = if inside { apart.rgb[(v - y) * vw + (u - x)] } else { [1, 2, 3] };
                    assert_eq!(*p, want, "{threads} threads, view at ({x}, {y}), pixel ({u}, {v})");
                }
            }
        }
    }

    #[test]
    fn a_warm_renderer_draws_what_a_cold_one_draws() {
        // Everything a renderer keeps between frames is a cache of what the
        // frame would compute anyway: the second frame of a renderer, and a
        // frame after another map, are the first frame of a new one.
        let bsp = fixtures::lightmapped_demo_room(100, 200);
        let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0; 3], 90.0);
        let pal = fixtures::ramp_palette();
        let cm: Vec<u8> = (0..light::COLORMAP_LEN).map(|i| (i % 256 + 3 * (i / 256)) as u8).collect();
        let mut styles = NEUTRAL_LIGHTSTYLE_SCALES;
        styles[1] = 0.5;
        let scene = Scene { colormap: Some(&cm), light_styles: &styles, ..Scene::new(&bsp, cam, 160, 120, &pal) };
        let cold = Renderer::new().render(&scene);
        let mut r = Renderer::new();
        r.render(&scene);
        assert_eq!(r.render(&scene).rgb, cold.rgb, "the second frame");
        let other = demo_room();
        r.render(&Scene::new(&other, cam, 160, 120, &pal));
        assert_eq!(r.render(&scene).rgb, cold.rgb, "after another map");
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

        let a = render_once(&Scene::new(&bsp, cam, 160, 120, &pal));
        let b = render_once(&Scene { time: 0.6, ..Scene::new(&bsp, cam, 160, 120, &pal) });

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
        let t0 = render_once(&Scene::new(&bsp, cam, 160, 120, &pal));
        let t1 = render_once(&Scene { time: 9.5, ..Scene::new(&bsp, cam, 160, 120, &pal) });
        assert_eq!(t0.rgb, t1.rgb, "ordinary walls must not animate with time");
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
            mips: Default::default(),
            anim: None,
        };
        let sky = MipTex {
            name: "sky1".into(),
            width: 256,
            height: 128,
            offsets: [0, 0, 0, 0],
            pixels: synthetic_sky_pixels(),
            mips: Default::default(),
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
}

