//! The frame — host.c's `Host_Frame` as the `step` export the page calls
//! once per display refresh: `Host_FilterTime`'s 72 fps gate, then the
//! active mode's client frame, then the rest of `SCR_UpdateScreen` (the menu
//! and console overlays) and `V_UpdatePalette`: the cshifts and gamma as
//! per-channel ramps the finished frame is packed through into the presented
//! framebuffer (`VID_ShiftPalette`).

use quake_rs::render::{self, build_gamma_table};

use crate::app::{build_demo_n, ensure_app};
use crate::bench::{self, Phase};
use crate::cl_demo::step_demo;
use crate::cl_walk::step_walk;
use crate::input::derive_key_move;
use crate::snd_dma::{SND_QUEUE, STOP_SND_QUEUE};

/// `Host_FilterTime` (host.c): the most a single frame may advance the game —
/// a longer real frame (a hitch, a backgrounded tab) is clamped to 0.1 s of
/// `host_frametime` while `realtime` still takes the whole elapsed time.
const HOST_FRAMETIME_MAX: f32 = 0.1;
/// `Host_FilterTime`'s lower clamp on `host_frametime` ("don't allow really
/// long or short frames").
const HOST_FRAMETIME_MIN: f32 = 0.001;
/// `Host_FilterTime`'s frame cap: no host frame runs until 1/72 s of real
/// time has passed since the last one ("framerate is too high").
const HOST_FRAME_INTERVAL: f64 = 1.0 / 72.0;
/// DEVIATION (the only one in the gate): a frame may run up to 1 ms before
/// the full 1/72 s. The C polls a free-running clock; this port only gets a
/// chance to run on a display refresh, so the gate sees whole vsync
/// intervals. On a 144 Hz display two of them come to 13.889 ms, a hair
/// either side of 1/72 s, and without slack the rate judders between 72 and
/// 48 fps. The slack has a window: above 0.56 ms a 75 Hz display (13.33 ms)
/// runs every refresh instead of every other; below 1.39 ms 240 Hz (3 vsyncs
/// = 12.5 ms) and 165 Hz (2 = 12.12 ms) stay gated, at 60 and 55 fps, instead
/// of 80 and 82.5. 1 ms sits mid-window, ~0.4 ms from either edge for
/// timestamp jitter. Rates per display are pinned by
/// `host_filter_time_caps_every_refresh_rate_at_a_steady_cadence`.
const HOST_FRAME_TOLERANCE: f64 = 0.001;

/// `Host_FilterTime` (host.c): given `realtime` (already advanced by this
/// call's raw time), decide whether a host frame runs. `None` = "framerate is
/// too high": do nothing this call. `Some(host_frametime)` = run a frame that
/// advances the game by the real time since the last frame, clamped to
/// [0.001, 0.1]; `oldrealtime` moves up to `realtime`, dropping any
/// overshoot, as the C does.
fn host_filter_time(realtime: f64, oldrealtime: &mut f64) -> Option<f32> {
    let elapsed = realtime - *oldrealtime;
    if elapsed < HOST_FRAME_INTERVAL - HOST_FRAME_TOLERANCE {
        return None;
    }
    *oldrealtime = realtime;
    Some((elapsed as f32).clamp(HOST_FRAMETIME_MIN, HOST_FRAMETIME_MAX))
}

/// The finished RGB frame into the presented RGBA framebuffer (`vid.buffer`
/// for the page's `ImageData`), each channel through its ramp when `ramps` is
/// given ([`render::cshift_ramps`]: the cshifts, then gamma), alpha 255.
/// `fb` takes `rgb`'s size (a no-op at a steady resolution, so its pointer
/// and allocation stay put) and is written in place, four bytes a pixel —
/// not `clear()` plus four `Vec::push`es, which cost ~5x as much (PERF_PLAN B1:
/// 2.41 -> 0.46 ms at 1280x800 in wasm).
fn pack_rgba(fb: &mut Vec<u8>, rgb: &[[u8; 3]], ramps: Option<&[[u8; 256]; 3]>) {
    fb.resize(rgb.len() * 4, 255);
    match ramps {
        None => {
            for (out, px) in fb.chunks_exact_mut(4).zip(rgb) {
                out.copy_from_slice(&[px[0], px[1], px[2], 255]);
            }
        }
        Some([r, g, b]) => {
            for (out, px) in fb.chunks_exact_mut(4).zip(rgb) {
                out.copy_from_slice(&[r[px[0] as usize], g[px[1] as usize], b[px[2] as usize], 255]);
            }
        }
    }
}

/// One call per display refresh: `dt` is the raw wall-clock time since the
/// previous call. Like `Host_Frame`, it all goes to `realtime`, then
/// [`host_filter_time`] decides whether a frame runs: at most 72 per second
/// (see [`HOST_FRAME_TOLERANCE`]), each advancing the game (world, demo,
/// `host_time`) by the time since the last one, clamped to [0.001, 0.1].
/// Returns 1 when a frame ran and the framebuffer holds it, 0 when the cap
/// skipped this call (the page then has nothing new to present).
///
/// `dt = 0` (or a non-finite / negative `dt`) is the tests' and automation's
/// frozen frame: it always renders, and neither the gate nor the game clock
/// moves.
#[no_mangle]
pub extern "C" fn step(dt: f32) -> i32 {
    // Guard a non-finite / negative dt so both clocks only move forward.
    let real_dt = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
    let mut ran = 0;
    ensure_app(|a| {
        // Advance realtime every call, even a skipped one (Host_FilterTime's
        // `realtime += time`): it drives the flashing cursors, which keep
        // animating over a frozen frame.
        a.realtime += real_dt as f64;
        let dt = if real_dt == 0.0 {
            0.0
        } else {
            match host_filter_time(a.realtime, &mut a.oldrealtime) {
                Some(frametime) => frametime,
                None => return,
            }
        };
        ran = 1;
        bench::frame_begin();
        // host_time: the menudot spinner (mode-independent, like realtime).
        a.clock += dt;
        let (w, h) = (a.render_w, a.render_h);
        // While the menu OR console is up (key_dest != key_game) gameplay input is
        // gated and single player pauses (Host_ServerFrame skips SV_Physics); the
        // dispatcher owns that state, so it tells step_walk. The attract demo is
        // client-side playback and keeps running (CL_GetMessage reads on).
        // The console takes priority over the menu.
        let menu_visible = a.menu.visible;
        let gate_gameplay = menu_visible || a.console.open;
        // Keep the menu's M_Menu_Save_f gate current: a local single-player game
        // is running when walk mode is live and not in intermission (`sv.active
        // && !cl.intermission && svs.maxclients == 1` — always 1 client here).
        let game_active =
            a.mode == 0 && a.walk.as_ref().map(|wk| wk.intermission == 0).unwrap_or(false);
        a.menu.set_game_active(game_active);
        // sv.active (New Game asks "Are you sure?" while a game runs).
        a.menu.set_server_active(a.mode == 0 && a.walk.is_some());
        // Derive this frame's bindings-driven keyboard input (CL_BaseMove over
        // keys.c's keybindings) and hand it to the walk; step_walk zeroes it
        // while gameplay is gated.
        let km = derive_key_move(&a.menu, &a.keys_held);
        let viewsize = a.menu.viewsize();
        if let Some(wk) = a.walk.as_mut() {
            wk.key_move = km;
            wk.viewsize = viewsize;
        }
        if let Some(d) = a.demo.as_mut() {
            d.viewsize = viewsize;
            // +showscores only reaches the game while it owns the keyboard.
            d.show_scores = km.showscores && !gate_gameplay;
        }
        // Host_EndGame on the demo's svc_disconnect -> CL_NextDemo: once a demo
        // has shown its last frame, the next of quake.rc's `startdemos demo1
        // demo2 demo3` starts (a demo that cannot be built leaves this one to
        // loop, step_demo's fallback). CL_PlayDemo_f's CL_Disconnect stops
        // every sound first.
        if a.mode == 1 && dt > 0.0 {
            let next = a.demo.as_ref().filter(|d| d.at_end()).map(|d| d.demonum + 1);
            if let Some(next) = next.and_then(build_demo_n) {
                SND_QUEUE.with(|q| q.borrow_mut().clear());
                STOP_SND_QUEUE.with(|q| q.borrow_mut().clear());
                let mut next = next;
                next.viewsize = viewsize;
                next.show_scores = km.showscores && !gate_gameplay;
                a.demo = Some(next);
            }
        }
        // Each mode returns its frame plus its colour shifts (`cl.cshifts`, in
        // order): the software V_UpdatePalette shift tints the WHOLE screen, so it
        // is applied as the frame is packed, after the HUD/menu/console, not just
        // over the 3D view.
        bench::lap(Phase::Input);
        let frame = if a.mode == 1 {
            a.demo.as_mut().map(|d| step_demo(d, dt, gate_gameplay, w, h))
        } else {
            a.walk.as_mut().map(|wk| step_walk(wk, dt, gate_gameplay, w, h))
        };
        let (mut img, cshifts) = match frame {
            Some((image, cshifts)) => (Some(image), cshifts),
            None => (None, Vec::new()),
        };
        // Con_Print: the frame's prints (svc_print) reach the console
        // scrollback too — the C keeps one text buffer, whose last lines are
        // the notify lines the mode drew.
        let printed = if a.mode == 1 {
            a.demo.as_mut().map(|d| d.notify.take_printed())
        } else {
            a.walk.as_mut().map(|wk| wk.notify.take_printed())
        };
        if let Some(text) = printed.filter(|t| !t.is_empty()) {
            a.console.print(&text);
        }

        // svc_sellscreen (cl_parse.c): the C ran `Cmd_ExecuteString("help")` —
        // pop the Help/Ordering menu (the shareware episode-end "order Quake"
        // pitch). The walk raised the flag during its step; the menu (owned
        // here, at the App level) opens on the Help screen for the next frame.
        if let Some(wk) = a.walk.as_mut() {
            if wk.pending_sellscreen {
                wk.pending_sellscreen = false;
                a.menu.open_help();
            }
        }

        // The main menu overlays WHATEVER is playing (walk OR the attract demo).
        // Drawn here in the dispatcher, after the active mode rendered its frame
        // and BEFORE packing to the framebuffer. Mirroring Quake's key_dest model
        // (M_Draw is a no-op when key_dest == key_console), the menu is suppressed
        // while the console is down — the console owns the screen+keyboard — so the
        // two never both show (and the console sits on top, matching SCR_UpdateScreen
        // drawing SCR_DrawConsole then M_Draw under mutual exclusion).
        // Uses the active mode's palette, host_time (the App clock) for the
        // menudot spinner and realtime for the flashing cursors.
        if menu_visible && !a.console.open {
            // Keep the Video Options "current mode" tracking the actual render
            // resolution (the framebuffer is the source of truth), so a boot /
            // New Game / `map` that changed the render size can't leave it stale.
            a.menu.sync_resolution(a.render_w as i32, a.render_h as i32);
            if let Some(img) = img.as_mut() {
                if let Some(palette) = a.active_palette() {
                    render::draw_menu(
                        img,
                        &a.menu,
                        &a.menu_pics,
                        a.conchars.as_ref(),
                        a.clock,
                        a.realtime,
                        palette,
                    );
                }
            }
        }
        bench::lap(Phase::Menu);

        // The console overlays everything and (per the gate above) replaces the menu
        // while open — matching Quake, where the menu and the drop-down console are
        // mutually exclusive via key_dest. It owns the keyboard while open. Uses the
        // active mode's palette and realtime for the input cursor flash
        // (Con_DrawInput). SCR_SetUpToDrawConsole slides it first: down to half
        // the screen while open, back up when closed (drawn until it is gone).
        a.console.slide(dt, w, h);
        if a.console.current() > 0.0 {
            if let Some(img) = img.as_mut() {
                if let Some(palette) = a.active_palette() {
                    render::draw_console(
                        img,
                        &a.console,
                        a.conback.as_ref(),
                        a.conchars.as_ref(),
                        palette,
                        a.realtime,
                    );
                }
            }
        }
        bench::lap(Phase::Console);

        // V_UpdatePalette runs LAST in SCR_UpdateScreen. V_CheckGamma (view.c):
        // rebuild the gamma table only when the cvar actually changed.
        let g = a.menu.gamma();
        if g != a.gamma_value {
            a.gamma_value = g;
            a.gamma_table = build_gamma_table(g);
        }
        // The cshifts, then gamma, as per-channel ramps: the port's hardware-
        // palette boundary (VID_ShiftPalette). The fully composited frame (3D +
        // HUD + centerprint/notify + menu + console) maps through them as it
        // becomes the presented RGBA, which is the C's whole-palette shift for
        // every palette colour. No shift at gamma 1.0 (BuildGammaTable's
        // identity) skips the lookups: the default presentation is a copy.
        let ramps = if cshifts.is_empty() && a.gamma_value == 1.0 {
            None
        } else {
            Some(render::cshift_ramps(&cshifts, &a.gamma_table))
        };
        bench::lap(Phase::Blend);

        if let Some(img) = img {
            pack_rgba(&mut a.fb, &img.rgb, ramps.as_ref());
            // Presented: its buffer serves the next frame (render::recycle_image).
            render::recycle_image(img);
        }
        bench::lap(Phase::Pack);
        bench::frame_end();
    });
    ran
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{boot, APP};
    use crate::console::console_toggle;
    use crate::menu::{menu_cancel, menu_down, menu_left, menu_right, menu_select, menu_visible};
    use crate::test_util::*;

    // -- Host_FilterTime: realtime vs host_time ---------------------------------

    #[test]
    fn step_splits_realtime_from_host_time_like_host_filtertime() {
        // The page hands step() the RAW elapsed time. Like Host_FilterTime,
        // all of it goes to realtime (the flashing cursors' clock) while the
        // game clock (host_time: the menudot spinner, the world) advances by
        // host_frametime = min(dt, 0.1). No mode is booted: the clocks tick
        // regardless.
        let clocks = || {
            APP.with(|c| {
                let b = c.borrow();
                let a = b.as_ref().expect("step creates the app");
                (a.realtime, a.clock)
            })
        };
        step(0.5); // a half-second hitch
        let (rt, ht) = clocks();
        assert!((rt - 0.5).abs() < 1e-9, "realtime takes the whole hitch: {rt}");
        assert!((ht - 0.1).abs() < 1e-6, "host_time is capped at 0.1: {ht}");
        step(0.05); // a normal frame advances both alike
        let (rt, ht) = clocks();
        assert!((rt - 0.55).abs() < 1e-6 && (ht - 0.15).abs() < 1e-6, "{rt} {ht}");
        // Garbage never runs either clock backwards.
        step(f32::NAN);
        step(-1.0);
        let (rt2, ht2) = clocks();
        assert_eq!((rt2, ht2), (rt, ht), "non-finite / negative dt is ignored");
    }

    /// Drive [`host_filter_time`] the way the page does: one call per rAF
    /// timestamp (ms, as the browser reports them), `dt` narrowed to the
    /// export's f32 and summed into an f64 `realtime`. Returns, per call,
    /// `Some(host_frametime)` when a frame ran.
    fn gate_run(stamps_ms: &[f64]) -> Vec<Option<f32>> {
        let (mut realtime, mut oldrealtime, mut last) = (0.0f64, 0.0f64, 0.0f64);
        stamps_ms
            .iter()
            .map(|&now| {
                let dt = ((now - last) / 1000.0).max(0.0) as f32;
                last = now;
                realtime += dt as f64;
                host_filter_time(realtime, &mut oldrealtime)
            })
            .collect()
    }

    /// The call indices at which a frame ran.
    fn ran_at(runs: &[Option<f32>]) -> Vec<usize> {
        runs.iter().enumerate().filter(|(_, r)| r.is_some()).map(|(i, _)| i).collect()
    }

    /// A tiny deterministic LCG in [0, 1) for jittered timestamps.
    fn lcg(seed: &mut u32) -> f64 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (*seed >> 8) as f64 / (1u32 << 24) as f64
    }

    #[test]
    fn host_filter_time_caps_every_refresh_rate_at_a_steady_cadence() {
        // (display Hz, vsyncs per host frame). Browsers report rAF stamps
        // coarsened to 0.1 ms without cross-origin isolation: round to that.
        // 144 Hz is the case the tolerance exists for (2 vsyncs = 13.889 ms,
        // a hair either side of 1/72 s): a steady 72, never 72/48 judder.
        let cases = [
            (60.0, 1),  // 60 fps: every refresh
            (75.0, 1),  // 75 fps: the tolerance's one overshoot of the cap
            (90.0, 2),  // 45 fps
            (100.0, 2), // 50 fps
            (120.0, 2), // 60 fps
            (144.0, 2), // 72 fps
            (165.0, 3), // 55 fps: 2 vsyncs (12.12 ms) are still too fast
            (240.0, 4), // 60 fps: 3 vsyncs (12.5 ms) are still too fast
            (360.0, 5), // 72 fps
        ];
        for (hz, k) in cases {
            let stamps: Vec<f64> = (1..=(hz as usize) * 10)
                .map(|i| ((i as f64 * 1000.0 / hz) * 10.0).round() / 10.0)
                .collect();
            let runs = gate_run(&stamps);
            let at = ran_at(&runs);
            assert_eq!(at[0], k - 1, "{hz} Hz: the first frame runs after {k} vsyncs");
            assert!(
                at.windows(2).all(|w| w[1] - w[0] == k),
                "{hz} Hz: every host frame is exactly {k} vsyncs apart"
            );
            let fps = at.len() as f64 / 10.0;
            assert!((fps - hz / k as f64).abs() < 0.2, "{hz} Hz: {fps} fps");
            // The game clock loses nothing: host_frametime sums to real time.
            let game: f64 = runs.iter().flatten().map(|&t| t as f64).sum();
            let real = stamps[*at.last().unwrap()] / 1000.0;
            assert!((game - real).abs() < 1e-3, "{hz} Hz: game {game} vs real {real}");
        }
    }

    #[test]
    fn host_filter_time_holds_its_cadence_through_jitter_and_dropped_frames() {
        let mut seed = 0x5eed_u32;
        // 144 Hz with +-0.3 ms of timestamp jitter, then 0.1 ms coarsening:
        // still exactly every other refresh.
        let stamps: Vec<f64> = (1..=1440)
            .map(|i| {
                let t = i as f64 * 1000.0 / 144.0 + (lcg(&mut seed) - 0.5) * 0.6;
                (t * 10.0).round() / 10.0
            })
            .collect();
        let at = ran_at(&gate_run(&stamps));
        assert!(at.windows(2).all(|w| w[1] - w[0] == 2), "jittered 144 Hz stays at 72 fps");
        assert_eq!(at.len(), 720);

        // 60 Hz dropping every 7th refresh (a slow frame): every call runs,
        // and the long ones carry their whole 33 ms into the game.
        let mut stamps = Vec::new();
        let mut t = 0.0;
        for i in 1..=600 {
            t += if i % 7 == 0 { 2000.0 / 60.0 } else { 1000.0 / 60.0 };
            stamps.push(t);
        }
        let runs = gate_run(&stamps);
        assert!(runs.iter().all(|r| r.is_some()), "60 Hz never skips a refresh");
        assert!(runs.iter().flatten().any(|&f| (f - 2.0 / 60.0).abs() < 1e-4));

        // Irregular intervals (2..40 ms): a frame runs as soon as 1/72 s minus
        // the tolerance has passed since the last one, never earlier, and the
        // game clock (no interval reaches the 0.1 s clamp) matches real time.
        let mut stamps = Vec::new();
        let mut t = 0.0;
        for _ in 0..2000 {
            t += 2.0 + 38.0 * lcg(&mut seed);
            stamps.push(t);
        }
        let runs = gate_run(&stamps);
        let at = ran_at(&runs);
        let min = (HOST_FRAME_INTERVAL - HOST_FRAME_TOLERANCE) * 1000.0;
        let mut prev = 0.0;
        for (i, &now) in stamps.iter().enumerate() {
            let since = now - prev;
            // 1e-3 ms of slack for the f32 narrowing of each dt.
            if runs[i].is_some() {
                assert!(since >= min - 1e-3, "call {i} ran {since} ms after the last frame");
                prev = now;
            } else {
                assert!(since < min + 1e-3, "call {i} skipped {since} ms after the last frame");
            }
        }
        let game: f64 = runs.iter().flatten().map(|&t| t as f64).sum();
        assert!((game - stamps[*at.last().unwrap()] / 1000.0).abs() < 1e-3);
    }

    #[test]
    fn step_runs_72_frames_a_second_on_a_144hz_display_and_dt_zero_still_freezes() {
        assert_eq!(boot(), 1);
        close_menu();
        let clocks = || {
            APP.with(|c| {
                let b = c.borrow();
                let a = b.as_ref().unwrap();
                (a.clock, a.realtime, a.walk.as_ref().unwrap().clock)
            })
        };
        let (host0, real0, walk0) = clocks();
        let vsync = 1.0f32 / 144.0;
        let mut ran = 0;
        for _ in 0..144 {
            let before = clocks();
            let r = step(vsync);
            let after = clocks();
            if r == 0 {
                // A skipped call: realtime moves, nothing else does.
                assert_eq!((before.0, before.2), (after.0, after.2), "skipped call moved the game");
                assert!(after.1 > before.1, "realtime runs on every call");
            }
            ran += r;
        }
        assert_eq!(ran, 72, "one second at 144 Hz is 72 host frames");
        let (host1, real1, walk1) = clocks();
        assert!((real1 - real0 - 1.0).abs() < 1e-4, "realtime took the whole second");
        assert!((host1 - host0 - 1.0).abs() < 1e-3, "host_time lost nothing: {}", host1 - host0);
        assert!((walk1 - walk0 - 1.0).abs() < 1e-3, "the walk advanced 1 s: {}", walk1 - walk0);
        // dt = 0 is the automation's frozen frame: it always renders, and no
        // clock moves.
        assert_eq!(step(0.0), 1);
        assert_eq!(step(0.0), 1);
        assert_eq!(clocks(), (host1, real1, walk1));
    }

    #[test]
    fn step_draws_console_over_everything_when_open() {
        assert_eq!(boot(), 1);
        // Render one frame with the console CLOSED, then OPEN; the open frame must
        // differ (the panel paints over the top of the scene + menu).
        let (w, h) = APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            (a.render_w, a.render_h)
        });
        step(0.016);
        let closed: Vec<u8> = APP.with(|c| c.borrow().as_ref().unwrap().fb.clone());
        console_toggle();
        APP.with(|c| c.borrow_mut().as_mut().unwrap().console.println("test line"));
        step(0.016);
        let open: Vec<u8> = APP.with(|c| c.borrow().as_ref().unwrap().fb.clone());
        assert_eq!(closed.len(), w * h * 4);
        assert_ne!(closed, open, "the open console changes the rendered frame");
        // Pixels in the very top row (the panel) are present (non-uniform / drawn).
        let top_changed = closed[..w * 4] != open[..w * 4];
        assert!(top_changed, "the console panel paints the top region of the frame");
    }

    #[test]
    fn gamma_changes_the_presented_frame_and_one_is_byte_identity() {
        reset_queue();
        assert_eq!(boot(), 1);
        close_menu();
        let grab = || APP.with(|c| c.borrow().as_ref().unwrap().fb.clone());
        // dt=0 keeps the world/clock frozen, so back-to-back frames are
        // byte-identical and the ONLY variable below is the gamma LUT.
        step(0.0);
        let base = grab();
        step(0.0);
        assert_eq!(base, grab(), "dt=0 frames are deterministic");

        // Brightness right one notch (Options row 4): gamma 1.0 -> 0.95
        // (v_gamma.value -= dir * 0.05) — the presented bytes must change.
        menu_cancel();
        menu_down();
        menu_down();
        menu_select(); // -> Options
        for _ in 0..4 {
            menu_down(); // ROW_BRIGHTNESS
        }
        menu_right();
        menu_cancel();
        menu_cancel();
        assert_eq!(menu_visible(), 0);
        step(0.0);
        let bright = grab();
        assert_ne!(base, bright, "gamma 0.95 changes the presented frame");
        // No pixel got darker (the curve brightens everything below white).
        assert!(
            base.iter().zip(bright.iter()).all(|(a, b)| b >= a),
            "gamma < 1 must only brighten"
        );

        // Back to 1.0: the identity special case restores the EXACT bytes.
        menu_cancel(); // reopen (lands on Main)
        menu_down();
        menu_down();
        menu_select();
        for _ in 0..4 {
            menu_down();
        }
        menu_left(); // gamma 0.95 -> 1.0 (clamped at GAMMA_MAX)
        menu_cancel();
        menu_cancel();
        step(0.0);
        assert_eq!(base, grab(), "gamma 1.0 is a byte-exact identity");
    }
}
