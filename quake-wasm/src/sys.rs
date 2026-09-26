//! The program's main loop — what id's `sys_*.c` `main` does around
//! `Host_Frame`, for a program whose platform is a web page: it reads the
//! page's events from stdin and writes what the frame shows and plays to
//! stdout, in the records of [`crate::proto`].
//!
//! Each turn of the loop:
//!
//! 1. writes the turn's `State` and a `Sync` (and flushes): the host
//!    publishes everything since the last `Sync` to the page;
//! 2. reads events, applying each as it comes (keys, mouse, automation
//!    calls, the host's `AudioWake`s, on which it mixes the sound again),
//!    until the next `Tick` — blocking in the host while there is
//!    none, which is where the program waits for the display — or, while a
//!    `timedemo` runs, until the host's `End` (the host answers a polling
//!    `Sync` at once, so the demo runs frames back to back as `Host_Frame`
//!    does). A call is answered at once in a turn of its own, and so is a
//!    key that changed the page's UI state (the menu or the console opened,
//!    say), so the page's view of it is never more than a moment old;
//! 3. runs the host frame for the tick's `dt` ([`crate::host::step`]), mixes
//!    the sound (every tick, a host frame or not: [`crate::snd_dma`]) and
//!    writes its samples, then, when a frame ran, its picture.
//!
//! The loop is generic over the two streams, so the tests run the same
//! program on byte buffers (`web/PLATFORM.md` has the host's half).

use std::io::{self, Read, Write};
use std::time::Instant;

use crate::app::{boot_attract, APP};
use crate::automation;
use crate::cl_demo::timedemo_running;
use crate::config::{exec_config, Archived};
use crate::host::step;
use crate::input::{key_clear_states, key_event, mouse_move, pointer_unlocked};
use crate::proto::{
    read_event, AudioCounts, Event, Msg, FORMAT_RGBA8, PCM_CLEAR, STATE_BIND_GRAB, STATE_CONSOLE, STATE_MENU,
    STATE_TIMEDEMO, STATE_WALK,
};
use crate::savegame::scan_saves;
use crate::snd_dma::Audio;

/// Run the program: `quake.rc`'s startup, then turns until the host closes
/// the input.
pub(crate) fn run(mut input: impl Read, output: impl Write) -> io::Result<()> {
    let mut sys = Sys::new(output);
    sys.host_init();
    loop {
        let polling = timedemo_running() == 1;
        sys.end_turn(!polling)?;
        let Some(dt) = sys.read_turn(&mut input, polling)? else {
            return Ok(());
        };
        sys.frame(dt)?;
    }
}

/// What the loop keeps between turns.
struct Sys<W: Write> {
    out: W,
    /// The last tick consumed, echoed in every `Sync`.
    ack: u32,
    /// The sound device: the mixer and its clock.
    audio: Audio,
    /// A tick's samples as the `Pcm` record's little-endian bytes.
    pcm_bytes: Vec<u8>,
    /// The menu screen at the last frame, to rescan the saves when Load or
    /// Save opens.
    menu_screen: Option<i32>,
    /// `config.cfg` as last written or read.
    config: Option<Archived>,
    /// When the last frame started: a timedemo's frames time themselves.
    last_frame: Instant,
    /// The UI state the page last heard (`State`).
    state: (u32, i32),
}

impl<W: Write> Sys<W> {
    fn new(out: W) -> Sys<W> {
        Sys {
            out,
            ack: 0,
            audio: Audio::new(),
            pcm_bytes: Vec::new(),
            menu_screen: None,
            config: None,
            last_frame: Instant::now(),
            state: (u32::MAX, 0),
        }
    }

    /// `Host_Init`'s `quake.rc`: `exec config.cfg`, then `startdemos demo1
    /// demo2 demo3` (the attract loop; a key brings up the menu). The
    /// Load/Save listings are read once here too.
    fn host_init(&mut self) {
        self.config = exec_config();
        boot_attract();
        if self.config.is_none() {
            self.config = Archived::current();
        }
        scan_saves();
    }

    /// Close the turn: the page's UI state, then `Sync` — `wait` when the
    /// next turn waits for a tick — and flush, so the host publishes it.
    fn end_turn(&mut self, wait: bool) -> io::Result<()> {
        self.state = ui_state();
        let (flags, menu_screen) = self.state;
        Msg::State { flags, menu_screen }.write_to(&mut self.out)?;
        Msg::Sync { seq: self.ack, wait }.write_to(&mut self.out)?;
        self.out.flush()
    }

    /// Read and apply events until the turn's tick (its `dt`), or — when
    /// `polling` — until the host has nothing more (the time since the last
    /// frame). `None` when the input ended.
    fn read_turn(&mut self, input: &mut impl Read, polling: bool) -> io::Result<Option<f64>> {
        loop {
            let Some(event) = read_event(input)? else { return Ok(None) };
            match event {
                Event::Tick { seq, dt } => {
                    self.ack = seq;
                    if !polling {
                        return Ok(Some(dt));
                    }
                }
                Event::End if polling => return Ok(Some(self.last_frame.elapsed().as_secs_f64())),
                Event::End | Event::Unknown(_) => {}
                Event::Key { keynum, down, ch } => {
                    key_event(i32::from(keynum), i32::from(down), ch.min(i32::MAX as u32) as i32);
                    if ui_state() != self.state {
                        self.end_turn(!polling)?;
                    }
                }
                Event::Mouse { dx, dy } => mouse_move(dx, dy),
                Event::ClearKeys => key_clear_states(),
                Event::PointerUnlocked => pointer_unlocked(),
                Event::AudioReady { running, rate } => self.audio.device(running, rate),
                Event::AudioClock(pos) => self.audio.clock(pos),
                Event::AudioWake(pos) => {
                    // S_ExtraUpdate: top the device's ring up between frames.
                    self.audio.clock(pos);
                    self.write_sound(0.0)?;
                    self.out.flush()?;
                }
                Event::Call { id, line } => {
                    // The sound device's own calls, then the game's.
                    let answer = match self.audio.call(&line) {
                        Some((value, text)) => automation::Answer { value, text },
                        None => automation::call(&line),
                    };
                    Msg::Reply { id, value: answer.value, text: &answer.text }.write_to(&mut self.out)?;
                    // A call is a turn of its own: answer it now, still
                    // waiting (or polling) for the same tick.
                    self.end_turn(!polling)?;
                }
            }
        }
    }

    /// The host frame for `dt` seconds, and what it produced.
    fn frame(&mut self, dt: f64) -> io::Result<()> {
        self.scan_saves_on_entry();
        crate::bench::before_frame();
        self.last_frame = Instant::now();
        // The page's refresh time as the old `step(dt: f32)` export took it.
        let ran = step(dt as f32) != 0;
        // S_Update_ every tick, even one Host_FilterTime's 72 fps cap skipped
        // (id's S_ExtraUpdate mixed between frames too): the device's ring
        // stays fed. The samples go first, ahead of the frame's pixels.
        self.write_sound(dt)?;
        if !ran {
            // Nothing new to show.
            return Ok(());
        }
        self.write_picture()?;
        crate::bench::write_values(&mut self.out)?;
        Archived::write_if_changed(&mut self.config);
        Ok(())
    }

    /// `M_Menu_Load_f` / `M_Menu_Save_f` run `M_ScanSaves` as the screen
    /// opens: rescan when a key since the last frame opened one, before the
    /// frame draws it.
    fn scan_saves_on_entry(&mut self) {
        let screen = (crate::menu::menu_visible() != 0).then(crate::menu::menu_screen_id);
        let opened = screen.filter(|&s| matches!(s, 2 | 3) && self.menu_screen != Some(s));
        self.menu_screen = Some(screen.unwrap_or(-1));
        if opened.is_some() {
            scan_saves();
        }
    }

    /// `VID_Update`: the presented framebuffer.
    fn write_picture(&mut self) -> io::Result<()> {
        let out = &mut self.out;
        APP.with(|c| {
            let b = c.borrow();
            let Some(a) = b.as_ref() else { return Ok(()) };
            let (w, h) = (a.render_w as u16, a.render_h as u16);
            Msg::Frame { w, h, format: FORMAT_RGBA8, pixels: &a.fb }.write_to(out)
        })
    }

    /// The tick's sound ([`Audio::frame`]): its samples for the page's ring
    /// (`Pcm`), and the device's counts (`Audio`).
    fn write_sound(&mut self, dt: f64) -> io::Result<()> {
        let Some(pcm) = self.audio.frame(dt) else { return Ok(()) };
        self.pcm_bytes.clear();
        self.pcm_bytes.extend(pcm.samples.iter().flat_map(|v| v.to_le_bytes()));
        let (start, rate) = (pcm.start, pcm.rate);
        let flags = if pcm.clear { PCM_CLEAR } else { 0 };
        Msg::Pcm { start, rate, flags, pairs: &self.pcm_bytes }.write_to(&mut self.out)?;
        let s = self.audio.stats;
        let mode = u32::from(self.audio.mode() == quake_rs::snd::SoundMode::Modern);
        let counts = AudioCounts {
            rate,
            mode,
            starts: s.starts,
            local: s.local,
            stops: s.stops,
            clears: s.clears,
            painted: s.painted,
        };
        Msg::Audio(counts).write_to(&mut self.out)
    }
}

/// What the page's own UI needs of the game (the `State` record): the
/// flags, and the menu screen showing.
fn ui_state() -> (u32, i32) {
    let flags = [
        (crate::menu::menu_visible(), STATE_MENU),
        (crate::console::console_visible(), STATE_CONSOLE),
        (crate::app::in_walk_mode(), STATE_WALK),
        (crate::menu::menu_bind_grabbing(), STATE_BIND_GRAB),
        (timedemo_running(), STATE_TIMEDEMO),
    ]
    .iter()
    .filter(|(on, _)| *on != 0)
    .fold(0, |f, (_, bit)| f | bit);
    (flags, crate::menu::menu_screen_id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{encode, Record};

    /// Run the program on `input` and split what it wrote.
    fn run_on(input: &[u8]) -> Vec<Record> {
        let mut out = Vec::new();
        run(input, &mut out).expect("the program runs to the end of its input");
        Record::split(&out)
    }

    fn syncs(recs: &[Record]) -> Vec<(u32, u8)> {
        recs.iter().filter(|r| r.kind == Record::SYNC).map(|r| (r.u32_at(0), r.payload[4])).collect()
    }

    #[test]
    fn boots_into_the_attract_demo_and_answers_each_tick_with_a_frame() {
        let mut input = Vec::new();
        input.extend(encode::audio_ready(true, 44100));
        for seq in 1..=3 {
            input.extend(encode::audio_clock(735 * seq));
            input.extend(encode::tick(seq, 1.0 / 60.0));
        }
        let recs = run_on(&input);
        // Startup's sync (ack 0), then one per tick, each waiting for the next.
        assert_eq!(syncs(&recs), [(0, 1), (1, 1), (2, 1), (3, 1)]);
        let frames: Vec<&Record> = recs.iter().filter(|r| r.kind == Record::FRAME).collect();
        // 60 Hz under the 72 fps gate: every refresh is a host frame.
        assert_eq!(frames.len(), 3);
        let (w, h) = (frames[0].payload[0] as usize | (frames[0].payload[1] as usize) << 8, 600);
        assert_eq!(w, 960);
        assert_eq!(frames[0].payload.len(), 8 + w * h * 4);
        // The attract demo sounds: every tick's samples at the device's rate,
        // from the device's position on (50 ms ahead of it, then a 60th of a
        // second a tick), the first clearing the ring (S_Init).
        let pcm: Vec<&Record> = recs.iter().filter(|r| r.kind == Record::PCM).collect();
        assert_eq!(pcm.len(), 3);
        let fields = |r: &Record| (r.u32_at(0), r.u32_at(4), r.u32_at(8), (r.payload.len() - 12) / 4);
        assert_eq!(fields(pcm[0]), (735, 44100, PCM_CLEAR, 2205));
        assert_eq!(fields(pcm[1]), (735 + 2205, 44100, 0, 735));
        assert_eq!(fields(pcm[2]), (735 + 2205 + 735, 44100, 0, 735));
        let audio = recs.iter().rfind(|r| r.kind == Record::AUDIO).expect("the device's counts");
        assert_eq!((audio.u32_at(0), audio.u32_at(4)), (44100, 1), "the 2026 mixer at the device's rate");
    }

    #[test]
    fn the_sound_calls_answer_from_the_mixer() {
        let mut input = Vec::new();
        input.extend(encode::tick(1, 1.0 / 60.0));
        input.extend(encode::call(1, "snd_stats"));
        input.extend(encode::call(2, "snd_channels"));
        let recs = run_on(&input);
        let texts: Vec<String> = recs
            .iter()
            .filter(|r| r.kind == Record::REPLY)
            .map(|r| String::from_utf8_lossy(&r.payload[12..]).into_owned())
            .collect();
        assert!(texts[0].starts_with("rate=48000 mode=2026 "), "{}", texts[0]);
        assert!(texts[1].lines().all(|l| l.split(' ').count() == 9), "{}", texts[1]);
    }

    #[test]
    fn a_call_is_answered_at_once_in_its_own_turn() {
        let mut input = Vec::new();
        input.extend(encode::call(5, "boot"));
        input.extend(encode::call(6, "menu_visible"));
        input.extend(encode::call(7, "set_resolution 320 200"));
        input.extend(encode::tick(1, 0.0));
        let recs = run_on(&input);
        let replies: Vec<(u32, f64)> =
            recs.iter().filter(|r| r.kind == Record::REPLY).map(|r| (r.u32_at(0), r.f64_at(4))).collect();
        assert_eq!(replies, [(5, 1.0), (6, 1.0), (7, 0.0)]);
        // Each call's turn syncs without a tick (ack still 0).
        assert_eq!(syncs(&recs), [(0, 1), (0, 1), (0, 1), (0, 1), (1, 1)]);
        let frame = recs.iter().find(|r| r.kind == Record::FRAME).expect("the tick's frame");
        assert_eq!(&frame.payload[..4], &[64, 1, 200, 0], "320x200");
        // The state before the tick: the menu is up over the walk.
        let state = recs.iter().rev().find(|r| r.kind == Record::STATE).unwrap();
        assert_eq!(state.u32_at(0) & (STATE_MENU | STATE_WALK), STATE_MENU | STATE_WALK);
    }

    #[test]
    fn keys_reach_key_event_between_ticks() {
        let mut input = Vec::new();
        input.extend(encode::call(1, "boot"));
        // Escape closes the menu (M_Main_Key), a held `w` is +forward.
        input.extend(encode::key(27, true, 0));
        input.extend(encode::key(27, false, 0));
        input.extend(encode::key(b'w', true, 'w' as u32));
        input.extend(encode::call(2, "key_is_down 119"));
        input.extend(encode::call(3, "menu_visible"));
        let recs = run_on(&input);
        let replies: Vec<f64> = recs.iter().filter(|r| r.kind == Record::REPLY).map(|r| r.f64_at(4)).collect();
        assert_eq!(replies, [1.0, 1.0, 0.0]);
    }

    #[test]
    fn a_key_that_changes_the_ui_state_ends_a_turn_at_once() {
        let mut input = Vec::new();
        input.extend(encode::call(1, "boot"));
        input.extend(encode::call(2, "menu_cancel"));
        // `w` is +forward: nothing the page shows changes, no turn.
        input.extend(encode::key(b'w', true, 'w' as u32));
        input.extend(encode::key(b'w', false, 0));
        // Escape opens the menu: its own turn, with the new state.
        input.extend(encode::key(27, true, 0));
        input.extend(encode::key(27, false, 0));
        let recs = run_on(&input);
        let states: Vec<u32> = recs.iter().filter(|r| r.kind == Record::STATE).map(|r| r.u32_at(0)).collect();
        // Startup (the attract demo), the two calls' turns, Escape's.
        assert_eq!(states.len(), 4, "{states:?}");
        assert_eq!(states[2] & STATE_MENU, 0, "the menu closed by the call");
        assert_eq!(states[3] & STATE_MENU, STATE_MENU, "Escape's turn: the menu is up");
    }

    #[test]
    fn a_timedemo_polls_instead_of_waiting_for_ticks() {
        let mut input = Vec::new();
        input.extend(encode::call(1, "set_resolution 320 200"));
        input.extend(encode::call(2, "exec timedemo demo1"));
        input.extend(encode::tick(1, 1.0 / 60.0));
        // The host's answers to the polling syncs that follow.
        for _ in 0..5 {
            input.extend(encode::end());
        }
        let recs = run_on(&input);
        let s = syncs(&recs);
        // After the call starts the demo, the turns poll (wait 0) and each
        // End runs a frame of the demo.
        assert!(s.iter().skip(3).take(5).all(|&(_, wait)| wait == 0), "{s:?}");
        let frames = recs.iter().filter(|r| r.kind == Record::FRAME).count();
        assert!(frames >= 5, "a frame per End: {frames}");
    }
}
