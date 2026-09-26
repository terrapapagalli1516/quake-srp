//! The frame — host.c's `Host_Frame` as the `step` export the page calls
//! once per display refresh: `Host_FilterTime`'s 72 fps gate, then the
//! active mode's client frame, then the rest of `SCR_UpdateScreen` (the menu
//! and console overlays) and `V_UpdatePalette`: the cshifts and gamma as
//! per-channel ramps the finished frame is packed through into the presented
//! framebuffer (`VID_ShiftPalette`).

use quake_rs::render::{self, build_gamma_table};
use quake_rs::client::cl_input::derive_key_move;
use quake_rs::client::host::{host_filter_time, host_filter_time_uncapped};

use crate::app::ensure_app;
use crate::bench::{self, Phase};
use crate::cl_demo::{host_end_game, step_demo, step_timedemo};
use crate::cl_walk::step_walk;

/// [`host_filter_time`], or the same frame without the 72 fps cap — every
/// call runs, advancing the game by the time since the last frame under the
/// same [0.001, 0.1] clamps ([`host_filter_time_uncapped`]) — while a
/// `timedemo` runs (id's `cls.timedemo`), or with the `wasm_uncapped` extra on
/// (a departure, opt-in via Options > Web extras, default off). A 120/144 Hz
/// display then runs one host frame per refresh, as the port did before it
/// had the gate.
fn host_frame_time(realtime: f64, oldrealtime: &mut f64, uncapped: bool) -> Option<f64> {
    if !uncapped {
        return host_filter_time(realtime, oldrealtime);
    }
    Some(host_filter_time_uncapped(realtime, oldrealtime))
}

/// The `wasm_showfps` extra's measurement (a departure, opt-in via Options >
/// Web extras, default off): QuakeWorld's `SCR_DrawFPS` counter. Every
/// presented frame counts (`fps_count++`); once a second of `realtime` has
/// passed since the window opened (`lastframetime`), the window's rate
/// becomes the shown value (`lastfps`) and a new window opens. QW shows the
/// raw count; this divides it by the window's length (at least 1 s, and up
/// to a frame longer), so a steady 60 Hz reads 60 rather than 60/61.
#[derive(Debug, Default)]
pub(crate) struct ShowFps {
    /// Frames presented in the current window (`fps_count`).
    count: u32,
    /// `realtime` when the window opened (`lastframetime`).
    since: f64,
    /// The rate drawn (`lastfps`); 0 until the first window closes.
    shown: u32,
}

impl ShowFps {
    /// One presented frame at `realtime`.
    pub(crate) fn frame(&mut self, realtime: f64) {
        self.count = self.count.saturating_add(1);
        let window = realtime - self.since;
        if window >= 1.0 {
            self.shown = (self.count as f64 / window).round().min(9999.0) as u32;
            self.count = 0;
            self.since = realtime;
        }
    }

    /// The frame rate to draw.
    pub(crate) fn shown(&self) -> u32 {
        self.shown
    }
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

/// `SCR_SetUpToDrawConsole` + `SCR_DrawConsole`: slide the console (`dt`,
/// `host_frametime`) and draw it over `img` at its height.
fn console_layer(a: &mut crate::app::App, img: Option<&mut render::Image>, dt: f32) {
    a.console.slide(dt, a.render_w, a.render_h);
    if a.console.current() > 0.0
        && let (Some(img), Some(palette)) = (img, a.active_palette())
    {
        render::draw_console(img, &a.console, a.conback.as_ref(), a.conchars.as_ref(), palette, a.realtime);
    }
}

/// One call per display refresh: `dt` is the raw wall-clock time since the
/// previous call. Like `Host_Frame`, it all goes to `realtime`, then
/// [`host_filter_time`] decides whether a frame runs: at most 72 per second
/// (see [`HOST_FRAME_TOLERANCE`](quake_rs::client::host::HOST_FRAME_TOLERANCE)), each advancing the game (world, demo,
/// `host_time`) by the time since the last one, clamped to [0.001, 0.1].
/// Returns 1 when a frame ran and the framebuffer holds it, 0 when the cap
/// skipped this call (the page then has nothing new to present).
///
/// `dt = 0` (or a non-finite / negative `dt`) is the tests' and automation's
/// frozen frame: it always renders, and neither the gate nor the game clock
/// moves.
///
/// While a `timedemo` runs every call is a host frame playing the next
/// recorded message; the page then calls `step` back to back, each call's
/// `dt` the previous call's own duration (see `web/index.html`), so
/// `realtime` — the clock `CL_FinishTimeDemo` measures on — adds up the time
/// the frames took and not the page's pauses between batches of them.
pub(crate) fn step(dt: f32) -> i32 {
    // Guard a non-finite / negative dt so both clocks only move forward.
    let real_dt = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
    let mut ran = 0;
    ensure_app(|a| {
        // Advance realtime every call, even a skipped one (Host_FilterTime's
        // `realtime += time`): it drives the flashing cursors, which keep
        // animating over a frozen frame.
        a.realtime += real_dt as f64;
        let uncapped = a.menu.extras().uncapped || a.cls.timedemo;
        // `host_frametime`, the C's double: the server advances sv.time by it
        // exactly; everything else here times itself with its f32.
        let host_frametime = if real_dt == 0.0 {
            0.0
        } else {
            match host_frame_time(a.realtime, &mut a.oldrealtime, uncapped) {
                Some(frametime) => frametime,
                None => return,
            }
        };
        let dt = host_frametime as f32;
        ran = 1;
        // Every presented real frame counts toward the wasm_showfps readout
        // (counted whether or not it is shown, so switching it on reads true
        // from the first second); the automation's frozen frames do not.
        if real_dt > 0.0 {
            a.show_fps.frame(a.realtime);
        }
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
        // Host_EndGame on the demo's svc_disconnect: once a demo has shown its
        // last frame, CL_NextDemo plays the next of the `startdemos` loop
        // (quake.rc: demo1 demo2 demo3) — or, outside the loop, the client
        // disconnects. CL_PlayDemo_f's CL_Disconnect stops every sound first.
        // (A timedemo ends in its own frame, below.)
        if a.demoplayback() && !a.cls.timedemo && dt > 0.0 && a.demo.as_ref().is_some_and(|d| d.at_end()) {
            host_end_game(a);
        }
        // The renderer's options are built inside the client frame, under this
        // borrow: hand it the menu's Web extras (wasm_exactpersp) first.
        crate::extras::set_frame_extras(a.menu.extras());
        // The scaled-2-D extra is draw.rs state; the menu's value is the truth.
        quake_rs::draw::set_scaled_2d(a.menu.extras().scaled_2d);
        if let Some(wk) = a.walk.as_mut() {
            wk.key_move = km;
            wk.viewsize = viewsize;
        }
        if let Some(d) = a.demo.as_mut() {
            d.viewsize = viewsize;
            // +showscores only reaches the game while it owns the keyboard.
            d.show_scores = km.showscores && !gate_gameplay;
        }
        // Each mode returns its frame plus its colour shifts (`cl.cshifts`, in
        // order): the software V_UpdatePalette shift tints the WHOLE screen, so it
        // is applied as the frame is packed, after the HUD/menu/console, not just
        // over the 3D view.
        bench::lap(Phase::Input);
        let mut frame = None;
        if a.cls.timedemo && a.demoplayback() {
            // CL_GetMessage in a timedemo: this frame's message (the second
            // frame starts the clock), or the end of the demo — Host_EndGame
            // then finishes the timedemo (CL_FinishTimeDemo's line) and the
            // loop's next demo, if any, plays from this frame.
            let (framecount, realtime) = (a.host_framecount, a.realtime);
            a.cls.td.message(framecount, realtime);
            frame = a.demo.as_mut().and_then(|d| step_timedemo(d, dt, gate_gameplay, w, h));
            if frame.is_none() {
                host_end_game(a);
                if let Some(d) = a.demo.as_mut() {
                    d.viewsize = viewsize;
                    d.show_scores = km.showscores && !gate_gameplay;
                }
            }
        }
        if frame.is_none() {
            frame = if a.mode == 1 {
                a.demo.as_mut().map(|d| step_demo(d, dt, gate_gameplay, w, h))
            } else {
                a.walk.as_mut().map(|wk| step_walk(wk, host_frametime, gate_gameplay, w, h))
            };
        }
        let (mut img, cshifts) = match frame {
            Some((image, cshifts)) => (Some(image), cshifts),
            // Disconnected (con_forcedup): no view — V_RenderView draws
            // nothing and the console covers the screen, the menu over it.
            None if a.disconnected && a.palette.is_some() => {
                (Some(render::Image::new(w, h, [0, 0, 0])), Vec::new())
            }
            None => (None, Vec::new()),
        };
        // Con_Print: the frame's prints (svc_print) reach the console
        // scrollback too — the C keeps one text buffer, whose last lines are
        // the notify lines the mode drew (which have this text already).
        let printed = if a.mode == 1 {
            a.demo.as_mut().map(|d| d.notify.take_printed())
        } else {
            a.walk.as_mut().map(|wk| wk.notify.take_printed())
        };
        if let Some(text) = printed.filter(|t| !t.is_empty()) {
            a.console.print_notified(&text);
        }

        // svc_sellscreen (cl_parse.c): the C ran `Cmd_ExecuteString("help")` —
        // pop the Help/Ordering menu (the shareware episode-end "order Quake"
        // pitch). The walk raised the flag during its step; the menu (owned
        // here, at the App level) opens on the Help screen for the next frame.
        if a.walk.as_mut().is_some_and(|wk| std::mem::take(&mut wk.pending_sellscreen)) {
            a.m_menu_help();
        }

        // The wasm_showfps extra (off by default): QuakeWorld draws it with the
        // rest of the play-screen 2-D (SCR_DrawFPS, before Sbar_Draw, the
        // console and M_Draw — so the menu's fade dims it) and not on the
        // intermission/finale screens.
        if a.menu.extras().show_fps {
            let intermission = if a.mode == 1 {
                a.demo.as_ref().and_then(|d| d.demo.frames.get(d.idx)).map(|f| f.intermission != 0)
            } else {
                a.walk.as_ref().map(|wk| wk.intermission != 0)
            };
            if let (Some(false), Some(img), Some(cc), Some(palette)) =
                (intermission, img.as_mut(), a.conchars.as_ref(), a.active_palette())
            {
                let sb_lines = render::calc_refdef(w, h, viewsize, false).sb_lines;
                render::draw_fps(img, cc, palette, a.show_fps.shown(), sb_lines);
            }
        }

        // con_forcedup: with nothing playing the console covers the screen,
        // and SCR_UpdateScreen draws it (SCR_DrawConsole) before M_Draw puts
        // the menu over it.
        a.console.forced_up = a.disconnected;
        if a.console.forced_up {
            console_layer(a, img.as_mut(), dt);
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
            if let Some(img) = img.as_mut()
                && let Some(palette) = a.active_palette()
            {
                // M_Draw: over the console background while the console
                // is out (scr_con_current: forced up, disconnected),
                // else over the faded screen.
                if a.console.current() > 0.0 {
                    render::draw_menu_over_console(
                        img,
                        &a.menu,
                        &a.menu_pics,
                        a.conchars.as_ref(),
                        a.conback.as_ref(),
                        a.clock,
                        a.realtime,
                        palette,
                    );
                } else {
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
        if !a.console.forced_up {
            console_layer(a, img.as_mut(), dt);
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
        a.host_framecount += 1;
    });
    ran
}

#[cfg(test)]
mod tests {
    use super::*;
    use quake_rs::client::host::{HOST_FRAMETIME_MAX, HOST_FRAMETIME_MIN};
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

    // -- Web extras: wasm_uncapped, wasm_showfps --------------------------------

    #[test]
    fn host_frame_time_uncapped_runs_every_refresh_with_the_same_clamps() {
        // Off: exactly Host_FilterTime's gate.
        for stamps in [[5.0, 10.0, 15.0], [13.0, 26.0, 40.0]] {
            let (mut a, mut b) = (0.0, 0.0);
            for t in stamps {
                let t = t / 1000.0;
                assert_eq!(host_frame_time(t, &mut a, false), host_filter_time(t, &mut b));
                assert_eq!(a, b);
            }
        }
        // On: every refresh runs, whatever the display, and the game clock
        // sums to real time.
        for hz in [60.0f64, 75.0, 120.0, 144.0, 165.0, 240.0] {
            let (mut realtime, mut old, mut game) = (0.0f64, 0.0f64, 0.0f64);
            for _ in 0..hz as usize * 2 {
                realtime += (1.0 / hz) as f32 as f64;
                let f = host_frame_time(realtime, &mut old, true);
                game += f.expect("uncapped: every refresh is a host frame");
            }
            assert!((game - realtime).abs() < 1e-4, "{hz} Hz: game {game} vs real {realtime}");
        }
        // Host_FilterTime's clamps still hold: a hitch advances 0.1 s at most,
        // and a sliver at least 1 ms.
        let mut old = 0.0;
        assert_eq!(host_frame_time(0.5, &mut old, true), Some(HOST_FRAMETIME_MAX));
        assert_eq!(host_frame_time(0.5001, &mut old, true), Some(HOST_FRAMETIME_MIN));
        assert_eq!(old, 0.5001);
    }

    #[test]
    fn step_with_wasm_uncapped_runs_one_frame_per_refresh_and_off_restores_72() {
        assert_eq!(boot(), 1);
        close_menu();
        let walk_clock = || APP.with(|c| c.borrow().as_ref().unwrap().walk.as_ref().unwrap().clock);
        let second_at_144hz = || (0..144).map(|_| step(1.0 / 144.0)).sum::<i32>();
        assert_eq!(second_at_144hz(), 72, "default: id's 72 fps cap");
        crate::menu::set_extras(1);
        let w0 = walk_clock();
        assert_eq!(second_at_144hz(), 144, "wasm_uncapped: every refresh");
        assert!((walk_clock() - w0 - 1.0).abs() < 1e-3, "the world still advances 1 s a second");
        crate::menu::set_extras(0);
        assert_eq!(second_at_144hz(), 72, "off again: the cap is back");
    }

    #[test]
    fn show_fps_measures_presented_frames_a_second() {
        let run = |hz: f64, secs: usize, every: usize| {
            let mut s = ShowFps::default();
            let mut realtime = 0.0f64;
            let mut shown = Vec::new();
            for i in 1..=(hz as usize * secs) {
                realtime += (1.0 / hz) as f32 as f64;
                if i % every == 0 {
                    s.frame(realtime);
                    shown.push(s.shown());
                }
            }
            shown
        };
        let at60 = run(60.0, 3, 1);
        assert_eq!(at60[..59], [0; 59], "nothing to show before the first second");
        assert!(at60[60..].iter().all(|&f| f == 60), "a steady 60: {at60:?}");
        assert!(run(144.0, 3, 1)[150..].iter().all(|&f| f == 144), "uncapped 144 Hz");
        assert!(run(144.0, 3, 2)[80..].iter().all(|&f| f == 72), "the cap's 72 at 144 Hz");
        assert!(run(100.0, 3, 2)[60..].iter().all(|&f| f == 50), "the cap's 50 at 100 Hz");
    }

    #[test]
    fn wasm_showfps_draws_the_rate_bottom_right_above_the_status_bar_only_when_on() {
        assert_eq!(boot(), 1);
        close_menu();
        for _ in 0..90 {
            step(1.0 / 60.0);
        }
        let (w, h) = APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            assert_eq!(a.show_fps.shown(), 60, "60 Hz presents 60 frames a second");
            (a.render_w, a.render_h)
        });
        let grab = || APP.with(|c| c.borrow().as_ref().unwrap().fb.clone());
        // Frozen frames (dt = 0): only the readout can differ.
        step(0.0);
        let off = grab();
        crate::menu::set_extras(2);
        step(0.0);
        let on = grab();
        assert_ne!(off, on, "wasm_showfps draws");
        // " 60 FPS" at x w-64..w-8, y h-56..h-48 (viewsize 100: sb_lines 48):
        // the 2-D layer 1:1 as id draws it (the scaled-2-D extra is off).
        let (x0, x1) = (w - 64, w - 8);
        let (y0, y1) = (h - 56, h - 48);
        for (i, (a, b)) in off.chunks_exact(4).zip(on.chunks_exact(4)).enumerate() {
            if a != b {
                let (x, y) = (i % w, i / w);
                assert!((x0..x1).contains(&x) && (y0..y1).contains(&y), "changed ({x},{y})");
            }
        }
        crate::menu::set_extras(0);
        step(0.0);
        assert_eq!(grab(), off, "off again: id's frame, byte for byte");
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
        menu_cancel(); // reopen: Main, still on "Options" (m_main_cursor)
        menu_select(); // Options, still on Brightness (options_cursor)
        menu_left(); // gamma 0.95 -> 1.0 (clamped at GAMMA_MAX)
        menu_cancel();
        menu_cancel();
        step(0.0);
        assert_eq!(base, grab(), "gamma 1.0 is a byte-exact identity");
    }
}
