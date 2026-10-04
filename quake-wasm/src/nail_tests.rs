//! `r_nailbarrels` end to end on the shareware data: where `v_nail.mdl`'s
//! barrels end, and the nails drawn out of them, frame by frame, at every
//! frame rate and with every placement of the gun the settings make.

use std::rc::Rc;

use quake_rs::client::nailbarrels::{BARREL_MUZZLE, NAIL, NAILGUN, NailBarrels};
use quake_rs::client::{Vid, Walk, cl_main, host::host_filter_time_display, host_cmd};
use quake_rs::mdl::{Frame, Mdl};
use quake_rs::qrand::QRand;
use quake_rs::render::{self, FovMode, SbarLayout, VideoCvars};
use quake_rs::stepping::Stepping;

#[test]
fn the_nailguns_barrels_end_where_nailbarrels_says() {
    let pak = crate::common::pak().expect("the shareware pak");
    let mdl = Mdl::parse(&pak.read_file(NAILGUN).unwrap().unwrap()).unwrap();
    let (scale, origin) = (mdl.header.scale, mdl.header.scale_origin);
    let [x, y, z] = BARREL_MUZZLE;
    for (f, frame) in mdl.frames.iter().enumerate() {
        let Frame::Single(frame) = frame else { panic!("frame {f}: a single frame") };
        let verts: Vec<[f32; 3]> =
            frame.verts.iter().map(|t| std::array::from_fn(|i| f32::from(t.v[i]) * scale[i] + origin[i])).collect();
        for left in [true, false] {
            // Each barrel's front face: its vertices within 1.5 units of x.
            let face: Vec<&[f32; 3]> =
                verts.iter().filter(|v| (v[1] > 0.0) == left && (v[0] - x).abs() <= 1.5).collect();
            assert!(face.len() >= 8, "frame {f}: a front face on each side");
            let mid = |k: usize| {
                let (lo, hi) = face.iter().fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v[k]), hi.max(v[k])));
                (lo + hi) / 2.0
            };
            let side = if left { y } else { -y };
            assert!(
                (mid(1) - side).abs() < 0.2 && (mid(2) - z).abs() < 0.2,
                "frame {f}: centred at {}, {}",
                mid(1),
                mid(2)
            );
            // No part of the gun reaches past the face, but the firing
            // frames' muzzle flash, which points along the barrel's axis.
            for v in verts.iter().filter(|v| (v[1] > 0.0) == left && v[0] > x + 1.5) {
                assert!((v[1] - side).abs() < 2.5 && (v[2] - z).abs() < 2.5, "frame {f}: the flash at {v:?}");
            }
        }
    }
}

/// One of the slop picture's placements of the gun: Screen size, the wider
/// view, the status bar overlay.
#[derive(Clone, Copy, Debug)]
struct Look {
    rate: f64,
    viewsize: f32,
    fov: FovMode,
    sbar: SbarLayout,
}

impl Look {
    const DEFAULT: Look = Look { rate: 240.0, viewsize: 110.0, fov: FovMode::HorPlus, sbar: SbarLayout::Overlay };
}

/// What a burst of the nailgun on e1m1, facing the start room's wall about
/// 95 units out (each nail is gone before the next is fired), drew: for
/// every frame with a nail in flight, the nail's distance ahead of the eye,
/// how many of its pixels show, and — in the first frame any of a nail's
/// show — whether one is within 2 pixels of the gun's.
fn burst(look: Look, nails: NailBarrels) -> Vec<(f32, usize, Option<bool>)> {
    burst_with(look, nails, 4, &mut |_| {})
}

/// [`burst`] with the weapon `impulse`, every frame as drawn handed to `each`.
fn burst_with(
    look: Look,
    nails: NailBarrels,
    impulse: i32,
    each: &mut dyn FnMut(&render::Image),
) -> Vec<(f32, usize, Option<bool>)> {
    let pak = crate::common::pak().expect("the shareware pak");
    let _scaled = ScaledTwoD::on();
    let (w, h) = (640, 360);
    let vid = Vid {
        width: w,
        height: h,
        display_aspect: w as f64 / h as f64,
        persp_span: render::PerspSpan::Spans8,
        video: VideoCvars { fov_mode: look.fov, ..VideoCvars::MODERN },
        mip: render::MipCvars::DEFAULT,
    };
    // As seen, without the nails, without the nails or the gun: the same
    // game three times (only what the client has cached differs).
    let game = |hide: &[&str]| -> Walk {
        let mut sound = Vec::new();
        let mut wk = host_cmd::build_walk_map(pak.clone(), "maps/e1m1.bsp", &Rc::new(QRand::new()), &mut sound, 8192)
            .expect("e1m1");
        let mut said = Vec::new();
        host_cmd::run_game_command(&mut wk, "god", &["god"], &mut said, &mut sound);
        host_cmd::run_game_command(&mut wk, "impulse", &["impulse", "9"], &mut said, &mut sound);
        (wk.yaw, wk.viewsize, wk.sbar_layout, wk.stepping) = (0.0, look.viewsize, look.sbar, Stepping::Uncapped);
        wk.nailbarrels = nails;
        for m in hide {
            wk.model_cache.insert(m.to_string(), None);
        }
        wk
    };
    const SUPER_NAIL: &str = "progs/s_spike.mdl";
    let mut games = [game(&[]), game(&[NAIL, SUPER_NAIL]), game(&[NAIL, SUPER_NAIL, NAILGUN])];
    let (mut realtime, mut old, mut t) = (0.0, 0.0, 0.0);
    let mut seen = Vec::new();
    let (mut last, mut shown) = (f32::MAX, false);
    while t < 0.6 {
        realtime += 1.0 / look.rate;
        let Some(dt) = host_filter_time_display(realtime, &mut old) else { continue };
        let mut frames = Vec::new();
        for wk in &mut games {
            if (0.1..0.2).contains(&t) {
                wk.next_impulse = impulse; // (`impulse 9`'s waits for the first frame)
            }
            wk.in_attack = (0.35..0.5).contains(&t); // two shots, 0.1 s apart
            frames.push(cl_main::walk_frame(wk, dt, false, &vid).image);
        }
        t += dt;
        each(&frames[0]);
        let wk = &games[0];
        let vm = &wk.server.vm;
        let (eye, v_angle) = wk.server.player_view();
        let forward = quake_rs::math::angle_vectors(v_angle).0;
        let flying: Vec<f32> = (0..vm.num_edicts() as i32)
            .filter(|&e| !vm.is_free_edict(e) && vm.ent_str(e, vm.fo().model).contains("spike"))
            .map(|e| quake_rs::math::dot(quake_rs::math::sub(vm.ent_vec(e, vm.fo().origin), eye), forward))
            .collect();
        assert!(flying.len() <= 1, "one nail at a time: {flying:?}");
        let Some(&ahead) = flying.first() else { continue };
        if ahead < last {
            shown = false; // a new nail
        }
        last = ahead;
        let differ = |a: &render::Image, b: &render::Image, i: usize| a.pixels[i] != b.pixels[i];
        let pixels: Vec<usize> = (0..w * h).filter(|&i| differ(&frames[0], &frames[1], i)).collect();
        let touches = (!shown && !pixels.is_empty()).then(|| {
            pixels.iter().any(|&i| {
                let (x, y) = ((i % w) as i64, (i / w) as i64);
                (-2..=2).any(|dy| {
                    (-2..=2).any(|dx| {
                        let (gx, gy) = (x + dx, y + dy);
                        (0..w as i64).contains(&gx)
                            && (0..h as i64).contains(&gy)
                            && differ(&frames[1], &frames[2], (gy * w as i64 + gx) as usize)
                    })
                })
            })
        });
        shown |= !pixels.is_empty();
        seen.push((ahead, pixels.len(), touches));
    }
    seen
}

/// For the test's thread only (the 2-D layer's scale flag is a thread-local).
struct ScaledTwoD;

impl ScaledTwoD {
    fn on() -> ScaledTwoD {
        quake_rs::draw::set_scaled_2d(true);
        ScaledTwoD
    }
}

impl Drop for ScaledTwoD {
    fn drop(&mut self) {
        quake_rs::draw::set_scaled_2d(false);
    }
}

/// A nail is not drawn while it is more than its own length (10 units:
/// `spike.mdl`) inside the barrel, and is first drawn as it leaves: in the
/// frame that takes it past the muzzle at the latest, and touching the gun
/// whenever a frame moves it less than its own length (from 100 Hz; at 60
/// and 72 Hz a frame can take it 14 to 17 units at once, as in id's game).
fn leaves_the_barrel(look: Look, seen: &[(f32, usize, Option<bool>)]) {
    let inside = BARREL_MUZZLE[0] - 10.0;
    assert!(seen.iter().any(|s| s.0 > 0.0), "{look:?}: a nail flew");
    for &(ahead, px, _) in seen {
        assert!(ahead >= inside || px == 0, "{look:?}: {px} pixels of a nail {ahead} units out, in the barrel");
    }
    let firsts: Vec<(f32, bool)> = seen.iter().filter_map(|&(ahead, _, touches)| Some((ahead, touches?))).collect();
    assert_eq!(firsts.len(), 2, "{look:?}: both barrels' nails drawn");
    let step = 1000.0 / look.rate as f32;
    for (ahead, touches) in firsts {
        assert!(ahead <= BARREL_MUZZLE[0] + step + 1.0, "{look:?}: first drawn {ahead} units out");
        assert!(touches || step > 10.0, "{look:?}: first drawn {ahead} units out, apart from the gun");
    }
}

#[test]
fn nails_leave_the_barrels_at_every_frame_rate() {
    for rate in [60.0, 72.0, 120.0, 240.0, 480.0] {
        let look = Look { rate, ..Look::DEFAULT };
        leaves_the_barrel(look, &burst(look, NailBarrels::Barrels));
    }
}

#[test]
fn nails_leave_the_barrels_wherever_the_settings_put_the_gun() {
    for viewsize in [100.0, 110.0, 120.0] {
        for fov in [FovMode::HorPlus, FovMode::Classic] {
            for sbar in [SbarLayout::Overlay, SbarLayout::Classic] {
                let look = Look { viewsize, fov, sbar, ..Look::DEFAULT };
                leaves_the_barrel(look, &burst(look, NailBarrels::Barrels));
            }
        }
    }
}

#[test]
fn id_draws_them_beside_the_eye() {
    // The test has teeth: id's nails at 240 Hz are drawn 4 and 8 units out,
    // beside the gun.
    let seen = burst(Look::DEFAULT, NailBarrels::Classic);
    assert!(seen.iter().any(|&(ahead, px, _)| ahead < BARREL_MUZZLE[0] - 10.0 && px > 0), "{seen:?}");
}

#[test]
fn the_super_nailgun_is_drawn_as_ids() {
    // Its nails fly the centre line, where its barrels hide their first 20
    // units: left as they are, frame for frame.
    let drawn = |nails| {
        let mut frames = Vec::new();
        let seen = burst_with(Look::DEFAULT, nails, 5, &mut |img| frames.push(img.pixels.clone()));
        assert!(seen.iter().any(|&(_, px, _)| px > 0), "its nails drawn");
        frames
    };
    assert!(drawn(NailBarrels::Barrels) == drawn(NailBarrels::Classic));
}
