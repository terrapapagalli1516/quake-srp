//! The video settings beyond id's modes: sizes past `MAXWIDTH` x `MAXHEIGHT`,
//! a field of view that widens with the display (Hor+), and a sky whose
//! clouds glide at the display's rate.
//!
//! id's WinQuake never set a mode larger than 1280x1024 (`r_shared.h`), and
//! `R_ViewChanged` (`r_main.c`) spreads `fov` over the view's width whatever
//! its shape, so a wider view shows the same horizontal angle with less above
//! and below. Both are right for 1996 and both are kept as **Classic**
//! ([`VideoCvars::CLASSIC`], the default here, which every golden and oracle
//! run uses), with id's sky. The port's three extras are for 2026 displays:
//!
//! - [`FovMode::HorPlus`]: `fov` is the horizontal field of view of a 4:3
//!   screen; a wider screen keeps that screen's VERTICAL field of view and sees
//!   more at the sides (at 16:9, `fov 90` spans 106.3 degrees). A 4:3 screen, or
//!   a narrower one, is drawn exactly as Classic.
//! - [`VideoCvars::hires`]: views up to [`HIRES_MAXWIDTH`] x [`HIRES_MAXHEIGHT`]
//!   (8K), and the sizes id tied to the pixel count kept at their 320x200
//!   proportions at any resolution — particles ([`super::part`]) and the
//!   underwater warp ([`super::warp`], rendered at full resolution). At 320x200
//!   both are id's to the pixel.
//! - [`SkyScroll::Fluid`]: the sky's cloud layer scrolls by its exact offset,
//!   not id's whole texels ([`super::sky`]). A sky texel that was a pixel at
//!   320x200 is 6 at 1080p and 11 at 4K, where id's eight one-texel jumps a
//!   second are a visible lurch; the fluid clouds glide, still in whole texels
//!   of the sky's own, unfiltered.
//!
//! These are cvars in id's sense — settings the platform sets and the
//! renderer reads each frame, like `d_mipscale` ([`super::MipCvars`]): the
//! scene hands them in with every frame
//! ([`RenderOptions::video`](super::RenderOptions::video)), and the client's
//! [`Vid`](crate::client::Vid) carries them from the platform.

use super::sky::SkyScroll;

/// id's widest and tallest view (`r_shared.h`: `MAXWIDTH` 1280, `MAXHEIGHT`
/// 1024): `vid_win.c` and `vid_ext.c` offer no larger mode, and the renderer
/// sizes its tables by them (`newedges[MAXHEIGHT]`, `d_scantable`, the warp's
/// `column[MAXWIDTH+AMP2*2]`). Classic renders at most this size
/// ([`VideoCvars::clamp_to_max`]); the port's tables are sized at run time and its edge
/// renderer's `u` is wider than id's 12.20 int (which wraps from 2048 wide),
/// so with [`VideoCvars::hires`] only [`HIRES_MAXWIDTH`] limits it.
pub const MAXWIDTH: usize = 1280;
/// See [`MAXWIDTH`].
pub const MAXHEIGHT: usize = 1024;

/// The largest view with [`VideoCvars::hires`]: 8K UHD, 7680x4320. Not a
/// limit of the renderer's arithmetic (its edge `u` is 44.20 fixed point) but
/// of memory: a frame is `w*h` palette indices plus a 16-bit z-buffer, 100 MB
/// here.
pub const HIRES_MAXWIDTH: usize = 7680;
/// See [`HIRES_MAXWIDTH`].
pub const HIRES_MAXHEIGHT: usize = 4320;

/// How `fov` (`scr_fov`, [`super::Camera::fov_deg`]) becomes the view's
/// horizontal field of view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FovMode {
    /// id's: `fov` spans the view's width (`R_ViewChanged`'s
    /// `horizontalFieldOfView = 2*tan(fov_x/2)`, `fov_x = scr_fov`).
    #[default]
    Classic,
    /// Hor+: `fov` spans the width of a 4:3 screen of the same height, and a
    /// wider screen extends it at the sides; the vertical field of view is
    /// the 4:3 screen's. A screen no wider than 4:3 is drawn as Classic (its
    /// view grows vertically instead, so a portrait screen is not a slit).
    HorPlus,
}

/// The widest horizontal field of view Hor+ widens to: `scr_fov`'s own cap
/// (`SCR_CalcRefdef` bounds it to 10..170).
const FOV_X_MAX: f64 = 170.0;

/// The display aspect Classic `fov` is defined on: DOS and Windows Quake's
/// modes all filled a 4:3 monitor (`vid.aspect`'s `320.0/240.0`).
const REFERENCE_ASPECT: f64 = 4.0 / 3.0;

impl FovMode {
    /// The horizontal field of view, in degrees, of a view on a
    /// `screen_w x screen_h` screen whose pixels have `vid.aspect`
    /// `pixel_aspect` (width over height as displayed, [`super::RenderOptions::pixel_aspect`]),
    /// for `fov` degrees (`scr_fov`). Classic returns `fov` itself, as id.
    ///
    /// Hor+ measures the SCREEN, not the view rectangle: below the status bar
    /// or inside the viewsize border the view is a part of the screen, and id's
    /// own 320x200 view above its status bar (320x152) is already wider than
    /// 4:3. With the screen's displayed aspect `D = screen_w*pixel_aspect /
    /// screen_h`, `tan(fov_x/2) = tan(fov/2) * D / (4/3)` when `D > 4/3`: the
    /// view has the `xscale` a 4:3 screen of the same height would have, so
    /// everything is drawn at the same size and only the sides are added.
    #[must_use]
    pub fn fov_x(self, fov: f32, screen_w: usize, screen_h: usize, pixel_aspect: f32) -> f32 {
        match self {
            FovMode::Classic => fov,
            FovMode::HorPlus => {
                if screen_w == 0 || screen_h == 0 || !(fov > 0.0 && fov < 180.0) {
                    return fov;
                }
                let d = screen_w as f64 * pixel_aspect as f64 / screen_h as f64;
                // A hair over 4:3 from float noise (960x600 at vid.aspect
                // 0.8333333 is 1.3333333) is 4:3: Classic, bit for bit. (A
                // NaN aspect is not wider either.)
                let wider = d > REFERENCE_ASPECT * (1.0 + 1e-6);
                if !wider {
                    return fov;
                }
                let t = (fov as f64 * 0.5).to_radians().tan() * d / REFERENCE_ASPECT;
                ((t.atan() * 2.0).to_degrees().min(FOV_X_MAX)) as f32
            }
        }
    }
}

/// The port's video cvars (the module docs). [`Default`] is
/// [`VideoCvars::CLASSIC`], id's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoCvars {
    /// How `fov` meets the display's shape.
    pub fov_mode: FovMode,
    /// Views past id's `MAXWIDTH` x `MAXHEIGHT`, with the resolution-bound
    /// sizes (particles, the underwater warp) at their 320x200 proportions.
    pub hires: bool,
    /// How the sky's clouds scroll: id's whole texels, or fluid
    /// (`r_fluidsky`).
    pub sky: SkyScroll,
}

impl VideoCvars {
    /// id's WinQuake: `fov` across the view, at most 1280x1024, id's sky.
    pub const CLASSIC: VideoCvars = VideoCvars { fov_mode: FovMode::Classic, hires: false, sky: SkyScroll::Classic };
    /// Every extra on: what a 2026 display wants.
    pub const MODERN: VideoCvars = VideoCvars { fov_mode: FovMode::HorPlus, hires: true, sky: SkyScroll::Fluid };
}

impl VideoCvars {
    /// The largest view these cvars allow: id's [`MAXWIDTH`] x [`MAXHEIGHT`],
    /// or [`HIRES_MAXWIDTH`] x [`HIRES_MAXHEIGHT`] with [`VideoCvars::hires`].
    /// The platform clamps the mode it asks for to this.
    #[must_use]
    pub fn max_view_size(self) -> (usize, usize) {
        if self.hires {
            (HIRES_MAXWIDTH, HIRES_MAXHEIGHT)
        } else {
            (MAXWIDTH, MAXHEIGHT)
        }
    }

    /// `(w, h)` limited to [`VideoCvars::max_view_size`]: in Classic id's
    /// largest view, as its video drivers never set a larger mode.
    #[must_use]
    pub fn clamp_to_max(self, w: usize, h: usize) -> (usize, usize) {
        let (mw, mh) = self.max_view_size();
        (w.min(mw), h.min(mh))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_is_id_fov_at_every_shape() {
        for (w, h, a) in [(320, 200, 1.0), (1920, 1080, 1.0), (960, 600, 0.8333333), (400, 1000, 1.0)] {
            assert_eq!(FovMode::Classic.fov_x(90.0, w, h, a), 90.0);
            assert_eq!(FovMode::Classic.fov_x(110.0, w, h, a), 110.0);
        }
    }

    #[test]
    fn hor_plus_keeps_the_4_3_vertical_and_widens() {
        // 4:3 in any form is Classic exactly: square 640x480, and every 16:10
        // preset shown at 4:3 (vid.aspect 0.8333333, 1.3333333 in f32).
        assert_eq!(FovMode::HorPlus.fov_x(90.0, 640, 480, 1.0), 90.0);
        for (w, h) in [(320, 200), (960, 600), (1280, 800)] {
            let a = crate::screen::vid_aspect(w, h, 4.0 / 3.0);
            assert_eq!(FovMode::HorPlus.fov_x(90.0, w, h, a), 90.0, "{w}x{h}");
        }
        // Narrower than 4:3 keeps fov across the width (the view grows up and down).
        assert_eq!(FovMode::HorPlus.fov_x(90.0, 1280, 1024, 1.0), 90.0);
        assert_eq!(FovMode::HorPlus.fov_x(90.0, 1080, 2340, 1.0), 90.0);
        // 16:9: tan(fov_x/2) = 1 * (16/9) / (4/3) = 4/3, so 106.26 degrees; the
        // same for a 16:10 mode shown at 16:9 (its pixels 1.111 wide).
        let wide = FovMode::HorPlus.fov_x(90.0, 1920, 1080, 1.0);
        assert!((wide - 106.260_2).abs() < 1e-3, "{wide}");
        let a = crate::screen::vid_aspect(960, 600, 16.0 / 9.0);
        assert!((FovMode::HorPlus.fov_x(90.0, 960, 600, a) - wide).abs() < 1e-3);
        // 21:9 (64:27) and 32:9; a big fov stops at scr_fov's 170.
        let uw = FovMode::HorPlus.fov_x(90.0, 2560, 1080, 1.0);
        assert!((uw - 121.284).abs() < 1e-2, "{uw}");
        assert!((FovMode::HorPlus.fov_x(90.0, 5120, 1440, 1.0) - 138.888).abs() < 1e-2);
        assert_eq!(FovMode::HorPlus.fov_x(160.0, 5120, 1440, 1.0), 170.0);
        // The same vertical field of view: xscale = (w/2)/tan(fov_x/2) is the
        // 4:3 screen's (1440x1080: 720).
        let t = (wide as f64 * 0.5).to_radians().tan();
        assert!((960.0 / t - 720.0).abs() < 1e-2);
    }

    #[test]
    fn hires_lifts_id_mode_limit() {
        assert_eq!(VideoCvars::CLASSIC.clamp_to_max(3840, 2160), (1280, 1024));
        assert_eq!(VideoCvars::CLASSIC.max_view_size(), (MAXWIDTH, MAXHEIGHT));
        let hires = VideoCvars { hires: true, ..VideoCvars::CLASSIC };
        assert_eq!(hires.clamp_to_max(3840, 2160), (3840, 2160));
        assert_eq!(hires.clamp_to_max(10_000, 10_000), (HIRES_MAXWIDTH, HIRES_MAXHEIGHT));
    }

    #[test]
    fn hor_plus_adds_columns_at_the_sides_of_the_4_3_picture() {
        // The same room at 4:3 (144x108, Classic) and 16:9 (192x108, Hor+):
        // the wide view's middle 144 columns are the 4:3 picture (the same
        // xscale, the centre 24 columns over), but for a few pixels on edges
        // the two clip differently.
        use crate::render::fixtures::render_once;
        use crate::render::{demo_room, Camera, RenderOptions, Scene};
        let bsp = demo_room();
        let pal = crate::render::fixtures::ramp_palette();
        let cam = Camera::looking_at([-200.0, -150.0, 40.0], [0.0, 0.0, 0.0], 90.0);
        let draw = |w: usize, h: usize, video: VideoCvars| {
            let options = RenderOptions { video, ..RenderOptions::default() };
            render_once(&Scene { options, ..Scene::new(&bsp, cam, w, h, &pal) })
        };
        let narrow = draw(144, 108, VideoCvars::CLASSIC);
        let wide = draw(192, 108, VideoCvars { fov_mode: FovMode::HorPlus, ..VideoCvars::CLASSIC });
        let classic_wide = draw(192, 108, VideoCvars::CLASSIC);
        let same = |img: &crate::render::Image| {
            (0..108 * 144).filter(|&i| img.pixels[(i / 144) * 192 + 24 + i % 144] == narrow.pixels[i]).count()
        };
        assert!(same(&wide) * 100 >= 98 * 144 * 108, "{} of {}", same(&wide), 144 * 108);
        // Classic spreads fov 90 over the 192 columns: a different picture.
        assert!(same(&classic_wide) * 100 < 90 * 144 * 108, "{}", same(&classic_wide));
    }
}
