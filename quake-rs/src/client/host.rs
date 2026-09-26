//! host.c's frame gate — `Host_FilterTime`: whether a host frame runs, and
//! how far it advances the game.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/host.c`.

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
    let elapsed = realtime - *oldrealtime;
    if elapsed < HOST_FRAMETIME_MIN {
        return None;
    }
    *oldrealtime = realtime;
    Some(elapsed.min(HOST_FRAMETIME_MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

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
