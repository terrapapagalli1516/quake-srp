//! `display HZ`: the screen a shot films, and the game's own frame gate on
//! each of its refreshes.
//!
//! The page runs the game on the display's refreshes (`requestAnimationFrame`).
//! On each, the gate decides whether a host frame runs: id's `Host_FilterTime`
//! with its 72 fps cap for `clock id` (the Classic preset's,
//! [`host_filter_time`]), the uncapped page's for `clock free` (a frame on
//! every refresh, [`host_filter_time_display`]). The screen shows the last
//! picture drawn until the next one. A film of it samples that screen: each
//! film frame shows what the screen shows at that moment.
//!
//! So `display 60` with `clock id` is 60 even frames a second, each 1/60 s of
//! game: a 16.7 ms refresh passes id's gate every time. `display 240` holds
//! each of id's pictures for 4 refreshes, since 3 come to 12.5 ms, under the
//! gate's 1/72 s less its 1 ms tolerance ([`HOST_FRAME_TOLERANCE`]): the 60
//! frames a second AUDIT's "Host loop" gives a 240 Hz display, not 72.
//!
//! The screen's clock is the game's real time, so `speed` slows the screen
//! with the world: `display 240` and `speed 0.25` in a 60 fps film is a
//! refresh every film frame, a 240 Hz screen at quarter speed (as `fps 240`
//! played at 60 is).
//!
//! [`HOST_FRAME_TOLERANCE`]: quake_rs::client::host::HOST_FRAME_TOLERANCE

use quake_rs::client::host::{host_filter_time, host_filter_time_display};

use super::shot::{Clock, Shot};

/// The rates `display HZ` takes. Below 10 Hz a refresh is longer than the
/// most a host frame may advance the game (`HOST_FRAMETIME_MAX`, 0.1 s), and
/// the game would fall behind its screen; 1000 Hz is the uncapped gate's
/// limit (`HOST_FRAMETIME_MIN`).
pub const RATES: std::ops::RangeInclusive<f64> = 10.0..=1000.0;

/// A screen refreshing `hz` times a second of the game's real time, and the
/// game's gate on each refresh.
#[derive(Clone, Debug)]
pub struct Screen {
    hz: f64,
    clock: Clock,
    /// The refreshes so far. Refresh 0 is film second 0's, which shows the
    /// warm-up's last frame.
    refreshes: u64,
    /// The refresh the last host frame ran on.
    last_frame: u64,
}

impl Screen {
    pub fn new(hz: f64, clock: Clock) -> Screen {
        Screen { hz, clock, refreshes: 0, last_frame: 0 }
    }

    /// The shot's screen, if it has a `display HZ` line.
    pub fn of(shot: &Shot) -> Option<Screen> {
        shot.refresh.map(|hz| Screen::new(hz, shot.clock()))
    }

    /// The host frames run by the refreshes up to game second `g` (the
    /// shot's [`Shot::game_time`]): for each refresh the gate passes, its
    /// game second and the `host_frametime` it steps. A refresh the gate
    /// holds runs nothing, and the screen shows the last picture again.
    pub fn frames_to(&mut self, g: f64) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        while (self.refreshes + 1) as f64 / self.hz <= g + 1e-9 {
            self.refreshes += 1;
            // The gate looks only at the time since its last frame. Given
            // that in whole refreshes, a 1000 Hz screen's 1 ms is exactly
            // the gate's 1 ms.
            let since = (self.refreshes - self.last_frame) as f64 / self.hz;
            let mut oldrealtime = 0.0;
            let ran = match self.clock {
                Clock::Id => host_filter_time(since, &mut oldrealtime),
                Clock::Free => host_filter_time_display(since, &mut oldrealtime),
            };
            if let Some(dt) = ran {
                self.last_frame = self.refreshes;
                out.push((self.refreshes as f64 / self.hz, dt));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(lines: &str) -> Shot {
        Shot::parse(&format!("map e1m1\nduration 2\n{lines}")).expect("parses")
    }

    /// Each film frame's host frames, as the film runs them: frame 0 none
    /// (it shows the warm-up's last), then the refreshes up to each frame.
    fn per_frame(s: &Shot) -> Vec<Vec<(f64, f64)>> {
        let mut screen = Screen::of(s).expect("a display line");
        (0..s.frames()).map(|n| screen.frames_to(s.game_time(n as f64 / s.fps))).collect()
    }

    /// The film frames each picture is held for: a run starts at each film
    /// frame with a host frame in it. The first and last runs are cut off by
    /// the film's ends and left out.
    fn holds(s: &Shot) -> Vec<usize> {
        let mut out = Vec::new();
        let mut run = 0;
        for f in per_frame(s).iter().skip(1) {
            if !f.is_empty() && run > 0 {
                out.push(run);
                run = 0;
            }
            run += 1;
        }
        out.remove(0);
        out
    }

    #[test]
    fn at_60_hz_and_id_s_clock_every_film_frame_is_a_new_game_frame() {
        // Classic on a 60 Hz screen: id's gate passes every 16.7 ms refresh,
        // so a 60 fps film of it is 60 even frames, each 1/60 s of game. (A
        // film with no `display` samples id's 72 Hz ticks at 60 a second
        // instead, and one tick in six never shows: a judder no 60 Hz screen
        // has.)
        let s = shot("fps 60\nclock id\ndisplay 60\n");
        let frames = per_frame(&s);
        assert_eq!(frames.len(), 120);
        assert!(frames[0].is_empty(), "frame 0 shows the warm-up's last");
        for (n, f) in frames.iter().enumerate().skip(1) {
            let [(g, dt)] = f.as_slice() else { panic!("frame {n}: {f:?}") };
            assert!((g - n as f64 / 60.0).abs() < 1e-12 && (dt - 1.0 / 60.0).abs() < 1e-12, "frame {n}: {g} {dt}");
        }
    }

    #[test]
    fn at_240_hz_id_s_gate_holds_each_picture_four_refreshes() {
        // A 240 Hz screen at quarter speed in a 60 fps film (a refresh every
        // film frame), and the same screen at 240 fps: id's gate passes every
        // 4th refresh, 60 game frames a second of 1/60 s each. Not 3s and 4s:
        // those are id's 72 Hz ticks laid on 240 refreshes (3.33 each), which
        // the gate never draws (it drops the overshoot, `oldrealtime =
        // realtime`).
        for lines in ["fps 60\nspeed 0.25\n", "fps 240\n"] {
            let s = shot(&format!("{lines}clock id\ndisplay 240\n"));
            let h = holds(&s);
            assert!(h.len() > 20 && h.iter().all(|&k| k == 4), "{lines:?}: {h:?}");
            assert!(per_frame(&s).iter().flatten().all(|(_, dt)| (dt - 1.0 / 60.0).abs() < 1e-12));
        }
        // The other rates as host.rs's own table has them: 144 Hz every 2nd
        // refresh (72 a second), 165 Hz every 3rd (55), 120 Hz every 2nd (60).
        for (hz, k) in [(144, 2), (165, 3), (120, 2), (360, 5)] {
            let s = shot(&format!("fps {hz}\nclock id\ndisplay {hz}\n"));
            let h = holds(&s);
            assert!(h.iter().all(|&x| x == k), "{hz} Hz: {h:?}");
        }
    }

    #[test]
    fn at_240_hz_the_uncapped_clock_draws_every_refresh() {
        let s = shot("fps 60\nspeed 0.25\nclock free\ndisplay 240\n");
        for (n, f) in per_frame(&s).iter().enumerate().skip(1) {
            let [(_, dt)] = f.as_slice() else { panic!("frame {n}: {f:?}") };
            assert!((dt - 1.0 / 240.0).abs() < 1e-12, "frame {n}: {dt}");
        }
        // Up to 1000 Hz, where the refresh is the gate's own 1 ms.
        let s = shot("fps 1000\nclock free\ndisplay 1000\n");
        assert!(per_frame(&s).iter().skip(1).all(|f| f.len() == 1), "every 1 ms refresh runs");
        // A screen slower than the film: a 60 Hz screen filmed at 240 fps
        // shows each of its refreshes for 4 film frames, whatever the clock.
        for clock in ["id", "free"] {
            let s = shot(&format!("fps 240\nclock {clock}\ndisplay 60\n"));
            assert!(holds(&s).iter().all(|&k| k == 4), "clock {clock}");
        }
    }

    #[test]
    fn display_is_a_shape_a_refresh_rate_or_both() {
        let s = shot("display 4:3\n");
        assert_eq!((s.display, s.refresh), (Some(Some(4.0 / 3.0)), None));
        let s = shot("display 240\n");
        assert_eq!((s.display, s.refresh), (None, Some(240.0)));
        let s = shot("display 4:3 60\ndisplay square\n");
        assert_eq!((s.display, s.refresh), (Some(None), Some(60.0)), "a later line sets only what it says");
        assert!(Screen::of(&shot("fps 60\n")).is_none(), "no display line: the film is the screen");
        for bad in ["display", "display 5", "display 2000", "display fast", "display 0:3"] {
            assert!(Shot::parse(&format!("map e1m1\nduration 2\n{bad}\n")).is_err(), "{bad}");
        }
    }
}
