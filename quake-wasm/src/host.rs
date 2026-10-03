//! The frame — host.c's `Host_Frame` as `step`, which the program's loop
//! runs once per display refresh (`sys`): `Host_FilterTime`'s 72 fps gate, then the
//! active mode's client frame, then the rest of `SCR_UpdateScreen` (the menu
//! and console overlays) and `V_UpdatePalette`: the cshifts and gamma as
//! per-channel ramps the finished frame is packed through into the presented
//! framebuffer (`VID_ShiftPalette`).

use quake_rs::render::{self, build_gamma_table};
use quake_rs::client::cl_input::derive_key_move;
use quake_rs::client::host::{host_filter_time, host_filter_time_display, host_filter_time_uncapped};
use quake_rs::stepping::Stepping;

use crate::app::ensure_app;
use crate::bench::{self, Phase};
use crate::cl_demo::{finish_host_error, finish_menu_credits, host_end_game, step_demo, step_timedemo};
use crate::cl_walk::step_walk;

/// How a host frame is gated and stepped: [`host_filter_time`]'s 72 fps cap
/// with id's per-frame code (Classic); every call without the cap while a
/// `timedemo` runs (id's `cls.timedemo`: [`host_filter_time_uncapped`]); or,
/// with `wasm_uncapped` (the 2026 profile), a frame on every display refresh
/// ([`host_filter_time_display`]) stepped as a run of id's 72 Hz frames
/// ([`Stepping::Uncapped`], `FRAMERATE.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameGate {
    Classic,
    Timedemo,
    Display,
}

impl FrameGate {
    fn new(uncapped: bool, timedemo: bool) -> FrameGate {
        match (timedemo, uncapped) {
            (true, _) => FrameGate::Timedemo,
            (false, true) => FrameGate::Display,
            (false, false) => FrameGate::Classic,
        }
    }

    /// The frame's `host_frametime`, or `None`: no frame this call.
    fn frame_time(self, realtime: f64, oldrealtime: &mut f64) -> Option<f64> {
        match self {
            FrameGate::Classic => host_filter_time(realtime, oldrealtime),
            FrameGate::Timedemo => Some(host_filter_time_uncapped(realtime, oldrealtime)),
            FrameGate::Display => host_filter_time_display(realtime, oldrealtime),
        }
    }

    /// How the game steps a frame of any length.
    fn stepping(self) -> Stepping {
        match self {
            FrameGate::Display => Stepping::Uncapped,
            FrameGate::Classic | FrameGate::Timedemo => Stepping::Classic,
        }
    }
}

/// The `wasm_showfps` measurement (a departure on Options > Classic / 2026's
/// settings page, off in both profiles): QuakeWorld's `SCR_DrawFPS` counter. Every
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


/// `SCR_SetUpToDrawConsole` + `SCR_DrawConsole`: slide the console (`dt`,
/// `host_frametime`) and draw it over `img` at its height.
fn console_layer(a: &mut crate::app::App, img: Option<&mut render::Image>, dt: f32) {
    a.console.slide(dt, a.render_w, a.render_h);
    if a.console.current() > 0.0
        && let Some(img) = img
    {
        render::draw_console(img, &a.console, a.conback.as_ref(), a.conchars.as_ref(), a.realtime);
    }
}

/// One call per display refresh: `dt` is the raw wall-clock time since the
/// previous call. Like `Host_Frame`, it all goes to `realtime`, then
/// [`host_filter_time`] decides whether a frame runs: at most 72 per second
/// (see [`HOST_FRAME_TOLERANCE`](quake_rs::client::host::HOST_FRAME_TOLERANCE)), each advancing the game (world, demo,
/// `host_time`) by the time since the last one, clamped to [0.001, 0.1].
/// A frame starts with `IN_Commands` (the gamepad's keys), and the live
/// game's move takes `IN_JoyMove`'s. Returns 1 when a frame ran and the
/// framebuffer holds it, 0 when the cap skipped this call (the page then has
/// nothing new to present).
///
/// `dt = 0` (or a non-finite / negative `dt`) is the tests' and automation's
/// frozen frame: it always renders, and neither the gate nor the game clock
/// moves.
///
/// While a `timedemo` runs every call is a host frame playing the next
/// recorded message; the program's loop then runs `step` back to back
/// without waiting for the display, each call's `dt` the time since the last
/// one started (`sys`), so `realtime` — the clock `CL_FinishTimeDemo`
/// measures on — adds up the time the frames took.
pub(crate) fn step(dt: f32) -> i32 {
    // Guard a non-finite / negative dt so both clocks only move forward.
    let real_dt = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
    let mut gated = None;
    ensure_app(|a| {
        // Advance realtime every call, even a skipped one (Host_FilterTime's
        // `realtime += time`): it drives the flashing cursors, which keep
        // animating over a frozen frame.
        a.realtime += real_dt as f64;
        let gate = FrameGate::new(a.settings.cvars.uncapped, a.cls.timedemo);
        // `host_frametime`, the C's double: the server advances sv.time by it
        // exactly; everything else here times itself with its f32.
        let frametime = if real_dt == 0.0 { Some(0.0) } else { gate.frame_time(a.realtime, &mut a.oldrealtime) };
        gated = frametime.map(|t| (gate, t));
    });
    let Some((gate, host_frametime)) = gated else { return 0 };
    // IN_Commands: the pad's buttons through Key_Event, before the frame's
    // commands and move, as host.c orders them.
    crate::input::in_commands();
    // Cbuf_Execute: right here in the C, after IN_Commands and before
    // CL_SendCmd. This port has no persistent cmd_text buffer, but a `wait`
    // inside a console command parks the rest of its line in
    // `App::pending_cmd` for exactly this moment (`execute_console_command`).
    // Taken (and run) OUTSIDE `ensure_app`: it calls back into
    // `execute_console_command`, which borrows the App itself.
    let pending = {
        let mut text = None;
        ensure_app(|a| text = a.pending_cmd.take());
        text
    };
    if let Some(text) = pending {
        crate::host_cmd::execute_console_command(&text);
    }
    let mut ran = 0;
    ensure_app(|a| {
        let stepping = gate.stepping();
        let dt = host_frametime as f32;
        ran = 1;
        // Every presented real frame counts toward the wasm_showfps readout
        // (counted whether or not it is shown, so switching it on reads true
        // from the first second); the automation's frozen frames do not.
        if real_dt > 0.0 {
            a.show_fps.frame(a.realtime);
        }
        bench::frame_begin(active_renderer(a));
        // host_time: the menudot spinner (mode-independent, like realtime).
        a.clock += host_frametime;
        // The settings' picture size and the renderer's per-thread settings.
        crate::vid::apply_settings(a);
        let (w, h) = (a.render_w, a.render_h);
        let vid = crate::vid::vid(a);
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
        let km = derive_key_move(&a.settings.cvars, &a.settings.binds, &a.keys_held);
        let viewsize = a.settings.cvars.viewsize;
        let crosshair = a.settings.cvars.crosshair;
        let sbar_layout = a.settings.cvars.sbar_layout;
        let show_fps = a.settings.cvars.show_fps;
        let lerpmove = a.settings.cvars.lerpmove;
        let lerpmodels = a.settings.cvars.lerpmodels;
        // Host_EndGame on the demo's svc_disconnect: once a demo has shown its
        // last frame, CL_NextDemo plays the next of the `startdemos` loop
        // (quake.rc: demo1 demo2 demo3) — or, outside the loop, the client
        // disconnects. CL_PlayDemo_f's CL_Disconnect stops every sound first.
        // (A timedemo ends in its own frame, below.)
        if a.demoplayback() && !a.cls.timedemo && dt > 0.0 && a.demo.as_ref().is_some_and(|d| d.at_end()) {
            host_end_game(a);
        }
        // The renderer's threads (`r_threads` against what the host offers),
        // to whichever game draws: every Walk and DemoPlay the host builds (a
        // boot, a load, the attract loop's next demo) draws on the setting
        // from its first frame.
        let threads = a.settings.cvars.threads.resolve(a.hw_threads);
        if let Some(wk) = a.walk.as_mut() {
            wk.key_move = km;
            wk.viewsize = viewsize;
            wk.crosshair = crosshair;
            wk.sbar_layout = sbar_layout;
            wk.show_fps = show_fps;
            wk.stepping = stepping;
            wk.lerpmove = lerpmove;
            wk.lerpmodels = lerpmodels;
            wk.renderer.set_threads(threads);
        }
        // CL_SendCmd's IN_Move: the pad's IN_JoyMove joins the keys' move.
        crate::input::in_joy_move(a, host_frametime, gate_gameplay);
        if let Some(d) = a.demo.as_mut() {
            d.renderer.set_threads(threads);
            d.viewsize = viewsize;
            d.crosshair = crosshair;
            d.sbar_layout = sbar_layout;
            d.show_fps = show_fps;
            d.stepping = stepping;
            d.lerpmove = lerpmove;
            d.lerpmodels = lerpmodels;
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
            frame = a.demo.as_mut().and_then(|d| step_timedemo(d, dt, gate_gameplay, &vid));
            if frame.is_none() {
                host_end_game(a);
                if let Some(d) = a.demo.as_mut() {
                    d.viewsize = viewsize;
                    d.crosshair = crosshair;
                    d.sbar_layout = sbar_layout;
                    d.show_fps = show_fps;
                    d.stepping = stepping;
                    d.lerpmove = lerpmove;
                    d.lerpmodels = lerpmodels;
                    d.show_scores = km.showscores && !gate_gameplay;
                }
            }
        }
        if frame.is_none() {
            frame = if a.mode == 1 {
                a.demo.as_mut().map(|d| step_demo(d, dt, gate_gameplay, &vid))
            } else {
                a.walk.as_mut().map(|wk| step_walk(wk, host_frametime, gate_gameplay, &vid))
            };
        }
        crate::input::rumble_after_frame(a);
        let (mut img, cshifts) = match frame {
            Some((image, cshifts)) => (Some(image), cshifts),
            // Disconnected (con_forcedup): no view — V_RenderView draws
            // nothing and the console covers the screen, the menu over it.
            None if a.disconnected && a.palette.is_some() => {
                (Some(render::Image::new(w, h, 0)), Vec::new())
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
        // A QuakeC error ended the game this frame: Host_Error's disconnect.
        finish_host_error(a);
        // The mission packs' re-release-only end-of-game credits roll:
        // builtin #79 finally true, `menu_credits` queued — end the session
        // like the Quit menu does.
        finish_menu_credits(a);

        // The wasm_showfps setting (off in both profiles): QuakeWorld draws it with the
        // rest of the play-screen 2-D (SCR_DrawFPS, before Sbar_Draw, the
        // console and M_Draw — so the menu's fade dims it) and not on the
        // intermission/finale screens. The port's sits in the top-left
        // corner, where the mode's notify lines made room for it (`show_fps`
        // above).
        if show_fps {
            let intermission = if a.mode == 1 {
                a.demo.as_ref().and_then(|d| d.demo.frames.get(d.idx)).map(|f| f.intermission != 0)
            } else {
                a.walk.as_ref().map(|wk| wk.intermission != 0)
            };
            if let (Some(false), Some(img), Some(cc)) = (intermission, img.as_mut(), a.conchars.as_ref()) {
                render::draw_fps(img, cc, a.show_fps.shown());
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
            crate::vid::sync_menu_resolution(a);
            if let Some(img) = img.as_mut() {
                // M_Draw: over the console background while the console
                // is out (scr_con_current: forced up, disconnected),
                // else over the faded screen.
                let clock = render::MenuClock { host_time: a.clock, realtime: a.realtime };
                let (menu, s, pics, cc) = (&a.menu, &a.settings, &a.menu_pics, a.conchars.as_ref());
                if a.console.current() > 0.0 {
                    render::draw_menu_over_console(img, menu, s, pics, cc, a.conback.as_ref(), clock);
                } else {
                    render::draw_menu(img, menu, s, pics, cc, clock);
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

        // SCR_ScreenShot_f, deferred to here: `img` is the finished 8-bit
        // frame (3D + HUD + menu/console overlays) exactly as id's
        // vid.buffer reads at this point of SCR_UpdateScreen — BEFORE
        // V_UpdatePalette's cshift/gamma DAC below, which is why id's
        // screenshots never show a flash or a gamma ramp. The console
        // command only raised the flag (nothing is rendered mid-command);
        // this is the next frame with something to write, and its own raw
        // palette ([`App::active_palette`], id's `host_basepal`) to write it
        // with.
        if a.screenshot_request {
            a.screenshot_request = false;
            if let (Some(im), Some(pal)) = (img.as_ref(), a.active_palette()) {
                match quake_rs::screen::screenshot_name(|n| crate::common::read_file(n).is_ok()) {
                    Some(name) => {
                        let bytes = quake_rs::screen::write_pcx(im.w, im.h, &im.pixels, pal);
                        match crate::common::write_file(&name, &bytes) {
                            Ok(()) => a.console.println(format!("Wrote {name}")),
                            Err(_) => a.console.println(format!("SCR_ScreenShot_f: couldn't write {name}")),
                        }
                    }
                    None => a.console.println("SCR_ScreenShot_f: Couldn't create a PCX file"),
                }
            }
        }

        // V_UpdatePalette runs LAST in SCR_UpdateScreen. V_CheckGamma (view.c):
        // rebuild the gamma table only when the cvar actually changed.
        let g = a.settings.cvars.gamma;
        if g != a.gamma_value {
            a.gamma_value = g;
            a.gamma_table = build_gamma_table(g);
        }
        // The cshifts, then gamma, over the palette: the frame's palette as
        // VID_ShiftPalette hands it to the DAC. The fully composited 8-bit
        // frame (3D + HUD + centerprint/notify + menu + console) is shown
        // through it, which tints the whole screen as the C's shift does.
        let palette = a.active_palette().map(|base| render::FramePalette::new(base, &cshifts, &a.gamma_table));
        bench::lap(Phase::Blend);

        // VID_Update: the frame to the page, as it asked for it (the
        // RGBA pack, when it takes RGBA, runs on the renderer's threads).
        match (img, palette) {
            (Some(img), Some(palette)) => a.present.frame(img, &palette, threads),
            (Some(img), None) => render::recycle_image(img),
            (None, _) => {}
        }
        bench::lap(Phase::Pack);
        bench::frame_end(active_renderer(a));
        a.host_framecount += 1;
    });
    ran
}

/// The renderer of the game that draws this frame: the demo's in mode 1,
/// else the walk's.
fn active_renderer(a: &mut crate::app::App) -> Option<&mut render::Renderer> {
    if a.mode == 1 {
        a.demo.as_mut().map(|d| &mut d.renderer)
    } else {
        a.walk.as_mut().map(|w| &mut w.renderer)
    }
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

    // -- the frame gate: Classic, timedemo, wasm_uncapped; wasm_showfps -------

    #[test]
    fn the_frame_gate_is_ids_classic_and_every_refresh_uncapped() {
        assert_eq!(FrameGate::new(false, false), FrameGate::Classic);
        assert_eq!(FrameGate::new(true, true), FrameGate::Timedemo, "a timedemo runs back to back");
        assert_eq!(FrameGate::new(true, false), FrameGate::Display);
        assert_eq!(
            [FrameGate::Classic, FrameGate::Timedemo, FrameGate::Display].map(FrameGate::stepping),
            [Stepping::Classic, Stepping::Classic, Stepping::Uncapped],
            "only the uncapped host steps the game as 72 Hz runs"
        );
        // Classic: exactly Host_FilterTime's gate.
        for stamps in [[5.0, 10.0, 15.0], [13.0, 26.0, 40.0]] {
            let (mut a, mut b) = (0.0, 0.0);
            for t in stamps {
                let t = t / 1000.0;
                assert_eq!(FrameGate::Classic.frame_time(t, &mut a), host_filter_time(t, &mut b));
                assert_eq!(a, b);
            }
        }
        // Uncapped: every refresh runs, whatever the display, and the game
        // clock sums to real time.
        for hz in [60.0f64, 75.0, 120.0, 144.0, 165.0, 240.0, 480.0] {
            let (mut realtime, mut old, mut game) = (0.0f64, 0.0f64, 0.0f64);
            for _ in 0..hz as usize * 2 {
                realtime += (1.0 / hz) as f32 as f64;
                let f = FrameGate::Display.frame_time(realtime, &mut old);
                game += f.expect("uncapped: every refresh is a host frame");
            }
            assert!((game - realtime).abs() < 1e-4, "{hz} Hz: game {game} vs real {realtime}");
        }
        // A hitch advances 0.1 s at most; a sliver under 1 ms waits for the
        // next call rather than running ahead of the clock (a timedemo's
        // frame is clamped up to 1 ms, as id's).
        let mut old = 0.0;
        assert_eq!(FrameGate::Display.frame_time(0.5, &mut old), Some(HOST_FRAMETIME_MAX));
        assert_eq!(FrameGate::Display.frame_time(0.5001, &mut old), None);
        assert_eq!(FrameGate::Timedemo.frame_time(0.5001, &mut old), Some(HOST_FRAMETIME_MIN));
        assert_eq!(old, 0.5001);
    }

    /// The 2026 profile in the frame: the game stepped as 72 Hz runs, the
    /// renderer's Hor+ and hires with native resolution, the 2-D layer at a
    /// whole scale, and the 2026 cross centred on the view, over the view and
    /// under the rest (the fade of the menu dims it); Classic is none of
    /// them.
    #[test]
    fn the_2026_profile_steps_uncapped_scales_2d_and_draws_the_crosshair() {
        use quake_rs::render::FovMode;
        let video_cvars = || APP.with(|c| crate::vid::vid(c.borrow().as_ref().unwrap()).video);
        assert_eq!(boot(), 1);
        close_menu();
        step(0.0);
        let stepping = || APP.with(|c| c.borrow().as_ref().unwrap().walk.as_ref().unwrap().stepping);
        assert_eq!(stepping(), Stepping::Classic);
        assert_eq!((video_cvars().fov_mode, video_cvars().hires, quake_rs::draw::scaled_2d()), (FovMode::Classic, false, false));
        use_2026();
        crate::vid::set_window(1600, 1000, 1.0);
        step(0.0);
        assert_eq!(stepping(), Stepping::Uncapped);
        assert_eq!((video_cvars().fov_mode, video_cvars().hires, quake_rs::draw::scaled_2d()), (FovMode::HorPlus, true, true));
        let (w, h) = APP.with(|c| {
            let b = c.borrow();
            (b.as_ref().unwrap().render_w, b.as_ref().unwrap().render_h)
        });
        assert_eq!((w, h), (1600, 1000), "native: the window's pixels");
        let with = APP.with(|c| c.borrow().as_ref().unwrap().present.rgba());
        crate::host_cmd::execute_console_command("crosshair 0");
        step(0.0);
        let without = APP.with(|c| c.borrow().as_ref().unwrap().present.rgba());
        // The view above the scaled status bar (viewsize 100: 48 rows x 5),
        // id's in either layout: the world drawn under it beside the bar
        // (2026's scr_sbaroverlay) leaves its centre where it was.
        let vrect = render::calc_refdef(w, h, 100.0, false, render::SbarLayout::Overlay).vrect;
        assert_eq!(vrect, render::calc_refdef(w, h, 100.0, false, render::SbarLayout::Classic).vrect);
        let (cx, cy) = (vrect.x + vrect.w / 2, vrect.y + vrect.h / 2);
        let differing: Vec<(usize, usize)> = (0..w * h)
            .filter(|&i| with[i * 4..i * 4 + 3] != without[i * 4..i * 4 + 3])
            .map(|i| (i % w, i / w))
            .collect();
        assert!(!differing.is_empty(), "the crosshair draws");
        // The cross's reach from the centre: half its thickness, the gap, an
        // arm and the outline (1000 rows: 2, 2, 7 and 1).
        let size = render::CrossSize::for_height(h);
        let reach = size.thickness / 2 + size.gap + size.arm + 1;
        assert_eq!(reach, 11);
        assert!(
            differing.iter().all(|&(x, y)| (cx - reach..cx + reach).contains(&x) && (cy - reach..cy + reach).contains(&y)),
            "only the cross, about the view's centre {cx},{cy}: {:?}",
            &differing[..differing.len().min(8)]
        );
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

    /// 2026's status bar overlay end to end, at 1920x1080 (pixel
    /// size 1, the 2-D layer at 5x, the bar 240 rows) and a wide frame
    /// (1315x535, 2x, 96 rows), on frozen frames of e1m1 in turn — id's, the
    /// overlay's twice, id's again: every pixel above the world under the
    /// view (the whole view, id's projection) is byte for byte id's; the bar
    /// is the same bar; each part beside it shows the world where id has the
    /// backtile; and an overlay frame leaves nothing behind (id's frame after
    /// it is id's frame). Then the same under water (the view in a water
    /// leaf, wobbled), where the wobble runs on into the corners.
    #[test]
    fn the_2026_overlay_draws_the_world_beside_the_bar_and_leaves_every_view_pixel_as_it_was() {
        assert_eq!(boot(), 1);
        close_menu();
        use_2026();
        let grab = || APP.with(|c| c.borrow().as_ref().unwrap().present.rgba());
        let frame = |on: bool| {
            crate::host_cmd::execute_console_command(if on { "scr_sbaroverlay 1" } else { "scr_sbaroverlay 0" });
            step(0.0);
            grab()
        };
        // The eye in water: the first point of a 7x7x7 grid over e1m1's water
        // leaves with water 24 units every way round it (quaketool shot's
        // `--liquid` search, shorter).
        let in_water = || {
            APP.with(|c| {
                let mut b = c.borrow_mut();
                let wk = b.as_mut().unwrap().walk.as_mut().unwrap();
                let (bsp, water) = (&wk.bsp, quake_rs::bsp::CONTENTS_WATER);
                let wet = |p: [f32; 3]| quake_rs::world::point_contents(bsp, p) == water;
                let deep = |p: [f32; 3]| {
                    wet(p) && (0..3).all(|k| [-24.0, 24.0].iter().all(|d| wet({ let mut q = p; q[k] += d; q })))
                };
                let eye = (bsp.leafs.iter().filter(|l| l.contents == water))
                    .flat_map(|l| {
                        let at = |k: usize, i: usize| l.mins[k] as f32 + (l.maxs[k] - l.mins[k]) as f32 * (i as f32 + 0.5) / 7.0;
                        (0..343).map(move |c| [at(0, c % 7), at(1, c / 7 % 7), at(2, c / 49)])
                    })
                    .find(|&p| deep(p))
                    .expect("e1m1 has water");
                wk.server.vm.ent_set_vector(wk.player, "origin", [eye[0], eye[1], eye[2] - 22.0]);
                let view = wk.server.player_view().0;
                assert_eq!(quake_rs::world::point_contents(&wk.bsp, view), water, "the view in water: warped");
            })
        };
        for underwater in [false, true] {
            if underwater {
                in_water();
            }
            for (ww, wh) in [(1920, 1080), (1315, 535)] {
                crate::vid::set_window(ww, wh, 1.0);
                step(0.0);
                let (w, h) = APP.with(|c| {
                    let b = c.borrow();
                    (b.as_ref().unwrap().render_w, b.as_ref().unwrap().render_h)
                });
                assert_eq!((w, h), (ww as usize, wh as usize), "Auto: one device pixel a pixel");
                let refdef = render::calc_refdef(w, h, 100.0, false, render::SbarLayout::Overlay);
                let below = refdef.below.expect("the view stands on the bar");
                let bar = render::status_bar_rect(w, h, refdef.sb_lines).expect("a bar");
                let (id, over, over2, id2) = (frame(false), frame(true), frame(true), frame(false));
                let what = format!("{w}x{h}{}", if underwater { " under water" } else { "" });
                assert_eq!(id2, id, "{what}: id's frame after the overlay's is id's");
                assert_eq!(over2, over, "{what}: the overlay's frame again is the same");
                let px = |img: &[u8], x: usize, y: usize| img[(y * w + x) * 4..(y * w + x) * 4 + 3].to_vec();
                assert!(over[..below.y * w * 4] == id[..below.y * w * 4], "{what}: every row of the view is id's");
                for y in bar.y..h {
                    for x in bar.x..bar.x + bar.w {
                        assert_eq!(px(&over, x, y), px(&id, x, y), "{what} ({x},{y}): the bar is the same bar");
                    }
                }
                let parts: Vec<_> = refdef.below_parts(Some(bar)).collect();
                assert!(parts.len() >= 2, "{what}: a corner either side of the bar: {parts:?}");
                for part in parts {
                    let differs = (part.y..part.y + part.h)
                        .flat_map(|y| (part.x..part.x + part.w).map(move |x| (x, y)))
                        .filter(|&(x, y)| px(&over, x, y) != px(&id, x, y))
                        .count();
                    assert!(differs * 2 > part.w * part.h, "{what}: {part:?} shows the world, not the backtile ({differs} differ)");
                }
                // Left of the view and right of it (id's even widths leave a
                // column or two in the wide frame): the backtile, as id's.
                for y in below.y..h {
                    for x in (0..below.x).chain(below.x + below.w..w) {
                        assert_eq!(px(&over, x, y), px(&id, x, y), "{what} ({x},{y}): beside the view, id's");
                    }
                }
            }
        }
        crate::host_cmd::execute_console_command("scr_sbaroverlay 1");
    }

    #[test]
    fn wasm_showfps_draws_the_rate_top_left_only_when_on() {
        assert_eq!(boot(), 1);
        close_menu();
        for _ in 0..90 {
            step(1.0 / 60.0);
        }
        let w = APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            assert_eq!(a.show_fps.shown(), 60, "60 Hz presents 60 frames a second");
            a.render_w
        });
        let grab = || APP.with(|c| c.borrow().as_ref().unwrap().present.rgba());
        // Frozen frames (dt = 0): only the readout can differ.
        step(0.0);
        let off = grab();
        crate::menu::set_extras(2);
        step(0.0);
        let on = grab();
        assert_ne!(off, on, "wasm_showfps draws");
        // " 60 FPS" at x 8..64, y 0..8, the top-left corner at the notify
        // lines' margin: the 2-D layer 1:1 as id draws it (the scaled-2-D
        // extra is off).
        let (x0, x1) = (8, 64);
        let (y0, y1) = (0, 8);
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
        let closed: Vec<u8> = APP.with(|c| c.borrow().as_ref().unwrap().present.rgba());
        console_toggle();
        APP.with(|c| c.borrow_mut().as_mut().unwrap().console.println("test line"));
        step(0.016);
        let open: Vec<u8> = APP.with(|c| c.borrow().as_ref().unwrap().present.rgba());
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
        let grab = || APP.with(|c| c.borrow().as_ref().unwrap().present.rgba());
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
