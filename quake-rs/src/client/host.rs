//! host.c's frame gate — `Host_FilterTime`: whether a host frame runs, and
//! how far it advances the game — and `Host_Error`, which ends it.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/host.c`.

use super::{SoundCall, Walk};
use crate::QError;

/// `Host_Error` (host.c) for an error in the local game — a QuakeC runtime
/// error, `Host_Error ("Program error")`: print what `PR_RunError` (or
/// `error`/`objerror`) printed and `Host_Error: Program error` to the console,
/// shut the server down, and disconnect, which stops every sound
/// (`CL_Disconnect`'s `S_StopAllSounds`). The walk keeps the message
/// ([`Walk::host_error`]) for the host, which finishes the job: it drops the
/// walk and stops the demo loop (`cls.demonum = -1`), and its console comes
/// down over the disconnected screen. Every preset does this; it is id's.
///
/// An error that is not a program error (the port's servers raise none
/// mid-game) is reported the same way, with its text as the message.
pub fn host_error(w: &mut Walk, e: &QError, sound: &mut Vec<SoundCall>) {
    let message = match e {
        QError::Program(pe) => {
            w.notify.print(&pe.console, w.host_time);
            "Program error".to_string()
        }
        other => other.to_string(),
    };
    w.notify.print(&format!("Host_Error: {message}\n"), w.host_time);
    w.host_error = Some(message);
    sound.push(SoundCall::StopAll);
}

/// `Host_FilterTime` (host.c): the most a single frame may advance the game —
/// a longer real frame (a hitch, a backgrounded tab) is clamped to 0.1 s of
/// `host_frametime` while `realtime` still takes the whole elapsed time.
pub const HOST_FRAMETIME_MAX: f64 = 0.1;
/// `Host_FilterTime`'s lower clamp on `host_frametime` ("don't allow really
/// long or short frames").
pub const HOST_FRAMETIME_MIN: f64 = 0.001;
/// `Host_FilterTime`'s frame cap: no host frame runs until 1/72 s of real
/// time has passed since the last one ("framerate is too high").
pub const HOST_FRAME_INTERVAL: f64 = 1.0 / 72.0;
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
pub const HOST_FRAME_TOLERANCE: f64 = 0.001;

/// `host_maxfps` (QuakeSpasm's name for it): the most host frames a second.
/// id's 72 is `Host_FilterTime`'s own gate, with id's timing
/// ([`host_filter_time`]): the Classic preset's, which the oracle proves.
/// [`FrameCap::NONE`] is a frame on every display refresh
/// ([`host_filter_time_display`]), the game stepped as id's 72 Hz frames
/// (`stepping`). The other caps keep that timing and only draw fewer frames
/// ([`host_filter_time_capped`]). On the console a number: 0 (none) or
/// 60..=240.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameCap(u32);

impl FrameCap {
    /// No cap: a frame every display refresh.
    pub const NONE: FrameCap = FrameCap(0);
    /// id's 72: `Host_FilterTime`'s gate.
    pub const ID: FrameCap = FrameCap(72);
    /// The lowest cap a number names: the bottom of the 60-480 Hz range the
    /// uncapped game is proven over (`quaketool framerate --check`).
    pub const MIN: u32 = 60;
    /// The highest.
    pub const MAX: u32 = 240;
    /// The menu's steps, left to right: 60, id's 72, 120, 144, 240, none.
    pub const STEPS: [FrameCap; 6] =
        [FrameCap::new(60), FrameCap::ID, FrameCap::new(120), FrameCap::new(144), FrameCap::new(240), FrameCap::NONE];

    /// `fps` frames a second at most, within [`FrameCap::MIN`]..=[`FrameCap::MAX`];
    /// 0 is none.
    pub const fn new(fps: u32) -> FrameCap {
        if fps == 0 {
            FrameCap::NONE
        } else if fps < FrameCap::MIN {
            FrameCap(FrameCap::MIN)
        } else if fps > FrameCap::MAX {
            FrameCap(FrameCap::MAX)
        } else {
            FrameCap(fps)
        }
    }

    /// The share of a cap's interval a refresh may come early by and still
    /// draw. A refresh's timestamp lands a hair either side of its place, so
    /// a cap equal to the display's rate, or a whole fraction of it, would
    /// drop a frame whenever two refreshes came a hair under 1/cap apart —
    /// 60 on a 120 Hz display (two refreshes, 16.67 ms) would judder between
    /// every second refresh and every third. 5% keeps them (15.83 ms there,
    /// 0.21 ms of slack at 240), and only a display under 5% faster than
    /// the cap still draws every refresh: a cap is at most 5% over itself.
    pub const TOLERANCE: f64 = 0.05;

    /// The least real time between two host frames under this cap: 1/cap,
    /// less [`FrameCap::TOLERANCE`] of it; none's is [`HOST_FRAMETIME_MIN`]
    /// (`host_filter_time_display`'s). id's 72 is [`host_filter_time`]'s
    /// own gate, not this.
    pub fn interval(self) -> f64 {
        if self == FrameCap::NONE { HOST_FRAMETIME_MIN } else { (1.0 - FrameCap::TOLERANCE) / f64::from(self.0) }
    }

    /// The cap a cvar value names: none for 0 (or less, or a word),
    /// otherwise the whole number within [`FrameCap::MIN`]..=[`FrameCap::MAX`].
    pub fn from_cvar(value: f32) -> FrameCap {
        if value.is_nan() || value < 1.0 { FrameCap::NONE } else { FrameCap::new(value.round().min(1e6) as u32) }
    }

    /// The cvar's value: the frames a second, 0 for none.
    pub fn cvar(self) -> u32 {
        self.0
    }

    /// Its place among the caps, lowest first, none after every number.
    fn order(self) -> u32 {
        if self == FrameCap::NONE { u32::MAX } else { self.0 }
    }

    /// The menu's next step from here, `step` +1 (right: more frames, then
    /// none) or -1, wrapping; from a number between the steps (set on the
    /// console), the step on that side of it.
    pub fn stepped(self, step: i32) -> FrameCap {
        let steps = FrameCap::STEPS;
        let n = steps.len() as i32;
        let at = match steps.iter().position(|&c| c == self) {
            Some(i) => i as i32 + step,
            None => {
                let above = steps.iter().position(|c| c.order() > self.order()).unwrap_or(steps.len()) as i32;
                if step > 0 { above } else { above - 1 }
            }
        };
        steps[at.rem_euclid(n) as usize]
    }
}

/// `Host_FilterTime` (host.c): given `realtime` (already advanced by this
/// call's raw time), decide whether a host frame runs. `None` = "framerate is
/// too high": do nothing this call. `Some(host_frametime)` = run a frame that
/// advances the game by the real time since the last frame, clamped to
/// [0.001, 0.1]; `oldrealtime` moves up to `realtime`, dropping any
/// overshoot, as the C does. `host_frametime` is the C's `double`: the server
/// advances `sv.time` by exactly it (`Server::client_frame_f64`); the client
/// frame's own timing takes it as an `f32`.
pub fn host_filter_time(realtime: f64, oldrealtime: &mut f64) -> Option<f64> {
    let elapsed = realtime - *oldrealtime;
    if elapsed < HOST_FRAME_INTERVAL - HOST_FRAME_TOLERANCE {
        return None;
    }
    *oldrealtime = realtime;
    Some(elapsed.clamp(HOST_FRAMETIME_MIN, HOST_FRAMETIME_MAX))
}

/// `Host_FilterTime` with the cap off — while `cls.timedemo` is set
/// (`if (!cls.timedemo && realtime - oldrealtime < 1.0/72.0)`), every call
/// runs a host frame, advancing the game by the real time since the last one
/// under the same [0.001, 0.1] clamps.
pub fn host_filter_time_uncapped(realtime: f64, oldrealtime: &mut f64) -> f64 {
    let elapsed = realtime - *oldrealtime;
    *oldrealtime = realtime;
    elapsed.clamp(HOST_FRAMETIME_MIN, HOST_FRAMETIME_MAX)
}

/// The uncapped host's gate (the port's own): `Host_FilterTime` with its cap
/// raised from 72 frames a second to the 1000 its lower clamp implies. A
/// frame runs on every display refresh up to 1000 Hz; a call less than
/// [`HOST_FRAMETIME_MIN`] after the last frame skips (`None`), leaving the
/// time to the next, where [`host_filter_time_uncapped`] would run it and
/// clamp it up to 1 ms — two refreshes 0.3 ms apart would advance the game
/// 2 ms, and the game would run ahead of the clock. With the cap off the
/// game advances exactly with real time between the clamps.
pub fn host_filter_time_display(realtime: f64, oldrealtime: &mut f64) -> Option<f64> {
    host_filter_time_capped(realtime, oldrealtime, FrameCap::NONE)
}

/// The capped host's gate (the port's own, `host_maxfps` 60..=240 but id's
/// 72): [`host_filter_time_display`] holding the frames to `cap`. A frame
/// runs on the first call at least [`FrameCap::interval`] after the last
/// one and advances the game by the whole time since, so the calls skipped
/// between lose none of it. The page calls once a display refresh, so a cap
/// that divides the display's rate draws every k-th refresh, evenly (60 on
/// 120 Hz: every second), one above it draws every refresh, and one that
/// does not divide it runs below itself (60 on 144 Hz: every third, 48). A
/// call between refreshes (the page's relaxed pacing asks for a frame the
/// moment a late one comes back) draws no sooner. [`FrameCap::NONE`] is
/// [`host_filter_time_display`] exactly.
pub fn host_filter_time_capped(realtime: f64, oldrealtime: &mut f64, cap: FrameCap) -> Option<f64> {
    let elapsed = realtime - *oldrealtime;
    if elapsed < cap.interval() {
        return None;
    }
    *oldrealtime = realtime;
    Some(elapsed.min(HOST_FRAMETIME_MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- host_maxfps --------------------------------------------------------

    /// `host_maxfps`: 0 (or less, or a word) is none; a number is held to
    /// 60..=240; the menu's steps go 60, 72, 120, 144, 240, none and round.
    #[test]
    fn a_frame_cap_is_none_or_60_to_240() {
        let caps = |vs: &[f32]| vs.iter().map(|&v| FrameCap::from_cvar(v).cvar()).collect::<Vec<_>>();
        assert_eq!(caps(&[0.0, -5.0, f32::NAN, 0.4, 1.0, 30.0, 60.0, 72.0, 99.6, 240.0, 1000.0, 1e30]), [0, 0, 0, 0, 60, 60, 60, 72, 100, 240, 240, 240]);
        assert_eq!((FrameCap::new(0), FrameCap::new(72), FrameCap::new(500)), (FrameCap::NONE, FrameCap::ID, FrameCap::new(240)));
        let mut cap = FrameCap::new(60);
        let right: Vec<u32> = (0..6).map(|_| { cap = cap.stepped(1); cap.cvar() }).collect();
        assert_eq!(right, [72, 120, 144, 240, 0, 60]);
        let left: Vec<u32> = (0..6).map(|_| { cap = cap.stepped(-1); cap.cvar() }).collect();
        assert_eq!(left, [0, 240, 144, 120, 72, 60]);
        assert_eq!((FrameCap::new(130).stepped(1), FrameCap::new(130).stepped(-1)), (FrameCap::new(144), FrameCap::new(120)));
        assert_eq!((FrameCap::new(200).stepped(1), FrameCap::new(61).stepped(-1)), (FrameCap::new(240), FrameCap::new(60)));
    }

    // -- Host_Error ---------------------------------------------------------

    /// A pak holding only what [`super::super::assemble_walk`] needs: the palette.
    fn palette_pak() -> crate::pak::Pak {
        let files: [(&str, Vec<u8>); 1] = [("gfx/palette.lmp", vec![0u8; 768])];
        let mut img = b"PACK".to_vec();
        let body: usize = files.iter().map(|f| f.1.len()).sum();
        img.extend_from_slice(&(12 + body as i32).to_le_bytes());
        img.extend_from_slice(&(64 * files.len() as i32).to_le_bytes());
        let mut dir = Vec::new();
        for (name, bytes) in &files {
            let mut n = [0u8; 56];
            n[..name.len()].copy_from_slice(name.as_bytes());
            dir.extend_from_slice(&n);
            dir.extend_from_slice(&(img.len() as i32).to_le_bytes());
            dir.extend_from_slice(&(bytes.len() as i32).to_le_bytes());
            img.extend_from_slice(bytes);
        }
        img.extend_from_slice(&dir);
        crate::pak::Pak::from_bytes("t".into(), img).expect("pak")
    }

    /// cl_main.rs:300 used to drop the server frame's `Result`, so a QuakeC
    /// error vanished and the game went on. id's `PR_RunError` prints its
    /// report and calls `Host_Error ("Program error")`, which ends the game:
    /// here the report and `Host_Error: Program error` reach the console
    /// text, every sound stops, the walk records the error for the host, and
    /// it runs nothing more.
    #[test]
    fn a_quakec_error_in_the_frame_is_host_error() {
        use crate::progs::{Op, Progs, Statement};
        use crate::server::testutil::{floor_bsp, player_progs_with_prethink, prime_player_globals};
        use crate::server::Server;
        // PlayerPreThink calls the function in global 57, which holds 0.
        let call_null = Statement { op: Op::Call0, a: 57, b: 0, c: 0 };
        let (img, g_const100, g_origin) = player_progs_with_prethink(vec![call_null]);
        let mut server = Server::new(floor_bsp(), Progs::parse(&img).expect("parse")).expect("server");
        prime_player_globals(&mut server, g_const100, g_origin);
        let player = server.connect_client().expect("connect");
        let mut w = super::super::assemble_walk(palette_pak(), "maps/t.bsp".into(), server, player, [0.0; 16], floor_bsp(), 0.0, 0.0)
            .expect("walk");
        let vid = super::super::Vid {
            width: 64,
            height: 40,
            display_aspect: 4.0 / 3.0,
            persp_span: crate::render::PerspSpan::Spans16,
            video: crate::render::VideoCvars::CLASSIC,
            mip: crate::render::MipCvars::DEFAULT,
        };

        let frame = super::super::cl_main::walk_frame(&mut w, 0.1, false, &vid);
        assert_eq!(w.host_error.as_deref(), Some("Program error"));
        let printed = w.notify.take_printed();
        assert!(printed.starts_with("CALL0      57(???)"), "PR_PrintStatement first: {printed:?}");
        assert!(printed.contains("             : PlayerPreThink\n"), "the stack trace: {printed:?}");
        assert!(printed.ends_with("\nNULL function\nHost_Error: Program error\n"), "{printed:?}");
        assert!(matches!(frame.sound[..], [SoundCall::StopAll]), "CL_Disconnect stops every sound");
        assert!(frame.image.pixels.iter().all(|&p| p == 0), "the disconnected screen");

        // The game is over: nothing more runs.
        let t = w.server.sv_time();
        let next = super::super::cl_main::walk_frame(&mut w, 0.1, false, &vid);
        assert_eq!(w.server.sv_time(), t);
        assert!(next.sound.is_empty() && w.notify.take_printed().is_empty());
    }

    // -- Host_FilterTime: realtime vs host_time ---------------------------------

    /// Drive [`host_filter_time`] the way the page does: one call per rAF
    /// timestamp (ms, as the browser reports them), `dt` narrowed to the
    /// export's f32 and summed into an f64 `realtime`. Returns, per call,
    /// `Some(host_frametime)` when a frame ran.
    fn gate_run(stamps_ms: &[f64]) -> Vec<Option<f64>> {
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
    fn ran_at(runs: &[Option<f64>]) -> Vec<usize> {
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
            let game: f64 = runs.iter().flatten().sum();
            let real = stamps[*at.last().unwrap()] / 1000.0;
            assert!((game - real).abs() < 1e-3, "{hz} Hz: game {game} vs real {real}");
        }
    }

    #[test]
    fn host_filter_time_display_runs_every_refresh_and_never_outruns_the_clock() {
        // Every refresh from 30 to 480 Hz is a frame of exactly its interval.
        for hz in [30.0, 60.0, 144.0, 240.0, 480.0] {
            let (mut old, mut game) = (0.0f64, 0.0f64);
            for i in 1..=(hz as usize) {
                let dt = host_filter_time_display(i as f64 / hz, &mut old).expect("every refresh runs");
                assert!((dt - 1.0 / hz).abs() < 1e-9, "{hz} Hz");
                game += dt;
            }
            assert!((game - 1.0).abs() < 1e-9, "{hz} Hz: {game}");
        }
        // Refreshes 0.25..1.75 ms apart (past 1000 Hz on average): the
        // uncapped timedemo gate clamps the short ones up and runs ahead of
        // real time; the display gate skips them and keeps time.
        let mut seed = 0x5eed_u32;
        let (mut t, mut old_a, mut old_b, mut game_a, mut game_b) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for _ in 0..4000 {
            t += 0.00025 + 0.0015 * lcg(&mut seed);
            game_a += host_filter_time_uncapped(t, &mut old_a);
            game_b += host_filter_time_display(t, &mut old_b).unwrap_or(0.0);
        }
        assert!(game_a > t * 1.1, "clamped up, the game outruns the clock: {game_a} vs {t}");
        assert!((game_b - t).abs() < 0.002, "the display gate keeps time: {game_b} vs {t}");
        // A hitch still costs what id's clamp costs: at most 0.1 s a frame.
        let mut old = 0.0;
        assert_eq!(host_filter_time_display(0.5, &mut old), Some(HOST_FRAMETIME_MAX));
    }

    /// Drive [`host_filter_time_capped`] once a refresh at `hz` for ten
    /// seconds, stamps coarsened to 0.1 ms as in [`gate_run`]: the refreshes
    /// a frame ran on, and the game time against the real time.
    fn capped_run(cap: FrameCap, hz: f64) -> (Vec<usize>, f64, f64) {
        let (mut realtime, mut old, mut last, mut game) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        let mut at = Vec::new();
        for i in 1..=(hz as usize) * 10 {
            let now = ((i as f64 * 1000.0 / hz) * 10.0).round() / 10.0;
            realtime += ((now - last) / 1000.0) as f32 as f64;
            last = now;
            if let Some(dt) = host_filter_time_capped(realtime, &mut old, cap) {
                at.push(i);
                game += dt;
            }
        }
        (at, game, old)
    }

    /// A cap draws on the first refresh at least its interval after the last
    /// frame: every k-th refresh, evenly, where it divides the display's
    /// rate; every refresh where it is above it; below itself where it does
    /// not divide it — and the game loses no time to the refreshes skipped.
    #[test]
    fn a_cap_draws_every_kth_refresh_evenly_and_keeps_time() {
        let cases = [
            (60, 120.0, 2), // a phone's panel at 120 Hz: every second refresh
            (60, 60.0, 1),
            (60, 144.0, 3), // 48 a second: 60 does not divide 144
            (120, 120.0, 1),
            (120, 240.0, 2),
            (120, 144.0, 2), // 72
            (144, 144.0, 1),
            (144, 120.0, 1), // above the display: every refresh
            (240, 60.0, 1),
            (240, 240.0, 1),
            (240, 360.0, 2),
        ];
        for (fps, hz, k) in cases {
            let (at, game, old) = capped_run(FrameCap::new(fps), hz);
            assert!(at.windows(2).all(|w| w[1] - w[0] == k), "{fps} on {hz} Hz: every {k}th refresh, evenly");
            let rate = at.len() as f64 / 10.0;
            assert!((rate - hz / k as f64).abs() < 0.2, "{fps} on {hz} Hz: {rate} a second");
            assert!((game - old).abs() < 1e-6, "{fps} on {hz} Hz: game {game} against real {old}");
        }
        // No cap: every refresh, as the display gate.
        let (at, _, _) = capped_run(FrameCap::NONE, 480.0);
        assert_eq!(at.len(), 4800);
    }

    /// 60 against a 120 Hz refresh holds every second refresh through
    /// +-0.3 ms of timestamp jitter, and a call between two refreshes (the
    /// page's relaxed pacing asking the moment a late frame comes back) draws
    /// no sooner than the cap's interval.
    #[test]
    fn a_cap_holds_through_jitter_and_calls_between_refreshes() {
        let cap = FrameCap::new(60);
        let mut seed = 0x5eed_u32;
        let (mut realtime, mut old, mut last) = (0.0f64, 0.0f64, 0.0f64);
        let mut at = Vec::new();
        for i in 1..=1200usize {
            let refresh = i as f64 * 1000.0 / 120.0 + (lcg(&mut seed) - 0.5) * 0.6;
            // A late frame's answer 3 ms after every frame's refresh.
            let calls = if at.last() == Some(&(i - 1)) { vec![refresh - 5.33, refresh] } else { vec![refresh] };
            for now in calls {
                realtime += ((now - last) / 1000.0) as f32 as f64;
                last = now;
                if host_filter_time_capped(realtime, &mut old, cap).is_some() {
                    assert_eq!(now, refresh, "refresh {i}: only a refresh draws");
                    at.push(i);
                }
            }
        }
        assert!(at.windows(2).all(|w| w[1] - w[0] == 2), "every second refresh: {:?}", &at[..8]);
        assert_eq!(at.len(), 600);
    }

    /// `host_frametime` is the C's double — `realtime - oldrealtime`, clamped
    /// with the double literals 0.001 and 0.1 (host.c) — not an f32 widened
    /// on the way to the server.
    #[test]
    fn host_frametime_is_the_double() {
        let mut old = 0.0;
        assert_eq!(host_filter_time(0.05, &mut old), Some(0.05));
        assert_ne!(0.05, f64::from(0.05f32));
        assert_eq!(host_filter_time(0.5, &mut old), Some(0.1));
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
        let game: f64 = runs.iter().flatten().sum();
        assert!((game - stamps[*at.last().unwrap()] / 1000.0).abs() < 1e-3);
    }
}
