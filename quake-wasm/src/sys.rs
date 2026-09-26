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
//!    calls), until the next `Tick` — blocking in the host while there is
//!    none, which is where the program waits for the display — or, while a
//!    `timedemo` runs, until the host's `End` (the host answers a polling
//!    `Sync` at once, so the demo runs frames back to back as `Host_Frame`
//!    does). A call is answered at once in a turn of its own, and so is a
//!    key that changed the page's UI state (the menu or the console opened,
//!    say), so the page's view of it is never more than a moment old;
//! 3. runs the host frame for the tick's `dt` ([`crate::host::step`]) and,
//!    when one ran, writes its picture, its sounds and the listener.
//!
//! The loop is generic over the two streams, so the tests run the same
//! program on byte buffers (`web/PLATFORM.md` has the host's half).

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::time::Instant;

use quake_rs::snd::SndParams;

use crate::app::{boot_attract, APP};
use crate::automation;
use crate::cl_demo::timedemo_running;
use crate::config::{exec_config, write_if_changed};
use crate::host::step;
use crate::input::{key_clear_states, key_event, mouse_move, pointer_unlocked};
use crate::proto::{
    read_event, Event, LoopWindow, Msg, Placement, FORMAT_RGBA8, STATE_BIND_GRAB, STATE_CONSOLE, STATE_FKEY,
    STATE_MENU, STATE_NATIVE, STATE_TIMEDEMO, STATE_WALK,
};
use crate::savegame::scan_saves;
use crate::snd_dma;

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
    /// Samples already sent, by content: a sample's bytes go out once
    /// (`Sample`), after which its sounds name it by id.
    samples: HashMap<(u64, usize), u32>,
    /// The sound generation the page last heard.
    generation: Option<i32>,
    /// The menu screen at the last frame, to rescan the saves when Load or
    /// Save opens.
    menu_screen: Option<i32>,
    /// `config.cfg` as last written or read.
    config: Option<String>,
    /// When the last frame started: a timedemo's frames time themselves.
    last_frame: Instant,
    /// The UI state the page last heard (`State`).
    state: UiState,
}

impl<W: Write> Sys<W> {
    fn new(out: W) -> Sys<W> {
        Sys {
            out,
            ack: 0,
            samples: HashMap::new(),
            generation: None,
            menu_screen: None,
            config: None,
            last_frame: Instant::now(),
            state: (u32::MAX, 0, 0),
        }
    }

    /// `Host_Init`'s `quake.rc`: `exec config.cfg`, then `startdemos demo1
    /// demo2 demo3` (the attract loop; a key brings up the menu). The
    /// Load/Save listings are read once here too.
    fn host_init(&mut self) {
        self.config = exec_config();
        boot_attract();
        if self.config.is_none() {
            self.config = crate::config::current_text();
        }
        scan_saves();
    }

    /// Close the turn: the page's UI state, then `Sync` — `wait` when the
    /// next turn waits for a tick — and flush, so the host publishes it.
    fn end_turn(&mut self, wait: bool) -> io::Result<()> {
        self.state = ui_state();
        let (flags, menu_screen, pixel_size) = self.state;
        Msg::State { flags, menu_screen, pixel_size }.write_to(&mut self.out)?;
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
                Event::AudioReady(on) => snd_dma::set_audio_ready(i32::from(on)),
                Event::Window { w, h } => crate::vid::set_window(w, h),
                Event::Call { id, line } => {
                    let answer = automation::call(&line);
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
        if step(dt as f32) == 0 {
            // Host_FilterTime's 72 fps cap skipped it: nothing new to show.
            return Ok(());
        }
        self.write_picture()?;
        self.write_sounds()?;
        crate::bench::write_values(&mut self.out)?;
        write_if_changed(&mut self.config);
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

    /// The frame's sound: a new generation first (`S_StopAllSounds` of a
    /// level change: everything queued after it belongs to the new level),
    /// then the one-shots, stops and menu clicks, the level's placed loops,
    /// and `S_Update`'s listener with the ambient levels and the volume.
    fn write_sounds(&mut self) -> io::Result<()> {
        let generation = snd_dma::sound_generation();
        if self.generation != Some(generation) {
            self.generation = Some(generation);
            Msg::Generation(generation as u32).write_to(&mut self.out)?;
            // S_Init's two ambient samples, started afresh for each level.
            for ch in 0..quake_rs::bsp::NUM_AMBIENTS {
                if let Some((wav, (start, end))) = snd_dma::ambient_sample(ch) {
                    let id = self.sample(&wav)?;
                    let window = LoopWindow { start, end };
                    Msg::Ambient { channel: ch as u32, id, window }.write_to(&mut self.out)?;
                }
            }
        }
        for (wav, p) in snd_dma::take_sounds() {
            let id = self.sample(&wav)?;
            Msg::Sound {
                id,
                at: placement(&p),
                entity: p.entity,
                channel: p.channel,
                view: p.is_view_entity,
                window: LoopWindow { start: p.loop_start, end: p.loop_end },
            }
            .write_to(&mut self.out)?;
        }
        for (entity, channel) in snd_dma::take_stop_sounds() {
            Msg::StopSound { entity, channel }.write_to(&mut self.out)?;
        }
        for wav in snd_dma::take_menu_sounds() {
            let id = self.sample(&wav)?;
            Msg::LocalSound { id }.write_to(&mut self.out)?;
        }
        for sl in snd_dma::take_static_sounds() {
            let id = self.sample(&sl.bytes)?;
            let window = LoopWindow { start: sl.loop_start, end: sl.loop_end };
            Msg::StaticSound { id, at: placement(&sl.params), window }.write_to(&mut self.out)?;
        }
        let l = snd_dma::listener();
        Msg::Listener {
            origin: l.pos,
            forward: l.forward,
            right: l.right,
            ambient: snd_dma::ambient_gains(),
            volume: snd_dma::volume(),
        }
        .write_to(&mut self.out)
    }

    /// The id of `wav`, sending its bytes first if the page has not had them.
    fn sample(&mut self, wav: &[u8]) -> io::Result<u32> {
        let key = (fnv1a64(wav), wav.len());
        if let Some(&id) = self.samples.get(&key) {
            return Ok(id);
        }
        let id = self.samples.len() as u32;
        self.samples.insert(key, id);
        Msg::Sample { id, wav }.write_to(&mut self.out)?;
        Ok(id)
    }
}

/// What the page's own UI needs of the game (the `State` record): the
/// flags, the menu screen showing, and the pixel size of a native picture.
type UiState = (u32, i32, u32);

/// The [`UiState`] now.
fn ui_state() -> UiState {
    let (native, fkey, pixel_size) = APP.with(|c| {
        c.borrow().as_ref().map_or((false, false, 0), |a| {
            let native = crate::vid::native(a);
            let pixel = a.window.filter(|_| native).map_or(0, |w| crate::vid::pixel_size(&a.settings.cvars, w));
            (native, a.settings.cvars.fkey, pixel)
        })
    });
    let flags = [
        (crate::menu::menu_visible() != 0, STATE_MENU),
        (crate::console::console_visible() != 0, STATE_CONSOLE),
        (crate::app::in_walk_mode() != 0, STATE_WALK),
        (crate::menu::menu_bind_grabbing() != 0, STATE_BIND_GRAB),
        (timedemo_running() != 0, STATE_TIMEDEMO),
        (native, STATE_NATIVE),
        (fkey, STATE_FKEY),
    ]
    .iter()
    .filter(|(on, _)| *on)
    .fold(0, |f, (_, bit)| f | bit);
    (flags, crate::menu::menu_screen_id(), pixel_size)
}

/// A sound's placement for the page's `SND_Spatialize`.
fn placement(p: &SndParams) -> Placement {
    Placement { origin: p.origin, volume: p.volume, attenuation: p.attenuation }
}

/// 64-bit FNV-1a: a sample's identity by content.
fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{encode, Record, STATE_FKEY, STATE_NATIVE};

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
        input.extend(encode::audio_ready(true));
        for seq in 1..=3 {
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
        // The attract demo plays: a new generation with its ambients, and
        // the listener every frame.
        assert!(recs.iter().any(|r| r.kind == Record::GENERATION));
        assert!(recs.iter().any(|r| r.kind == Record::AMBIENT));
        assert_eq!(recs.iter().filter(|r| r.kind == Record::LISTENER).count(), 3);
        // Samples go out once, before the first sound that names them.
        let first_sample = recs.iter().position(|r| r.kind == Record::SAMPLE);
        let first_ambient = recs.iter().position(|r| r.kind == Record::AMBIENT);
        assert!(first_sample < first_ambient);
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
        // Escape closes the menu (M_Main_Key), a held up arrow is +forward.
        input.extend(encode::key(27, true, 0));
        input.extend(encode::key(27, false, 0));
        input.extend(encode::key(quake_rs::keys::K_UPARROW, true, 0));
        input.extend(encode::call(2, "key_is_down 128"));
        input.extend(encode::call(3, "menu_visible"));
        let recs = run_on(&input);
        let replies: Vec<f64> = recs.iter().filter(|r| r.kind == Record::REPLY).map(|r| r.f64_at(4)).collect();
        assert_eq!(replies, [1.0, 1.0, 0.0]);
    }

    /// The 2026 profile's native resolution through the protocol: the page's
    /// `Window` in device pixels, the picture at a whole fraction of it (Auto:
    /// the smallest pixel that keeps a 1080p frame's cost), and the `State`
    /// that tells the page to fill its box with that pixel size. Classic
    /// shows its video mode in the 4:3 box whatever the window.
    #[test]
    fn the_window_sets_a_native_picture_in_2026_and_nothing_in_classic() {
        let frame_and_state = |profile: &str, win: (u32, u32)| {
            let mut input = Vec::new();
            input.extend(encode::call(1, &format!("exec profile {profile}")));
            input.extend(encode::window(win.0, win.1));
            input.extend(encode::tick(1, 0.0));
            input.extend(encode::tick(2, 0.0));
            let recs = run_on(&input);
            let frame = recs.iter().rev().find(|r| r.kind == Record::FRAME).expect("a frame");
            let (w, h) = (u16::from_le_bytes([frame.payload[0], frame.payload[1]]), u16::from_le_bytes([frame.payload[2], frame.payload[3]]));
            let state = recs.iter().rev().find(|r| r.kind == Record::STATE).unwrap();
            (w, h, state.u32_at(0) & (STATE_NATIVE | STATE_FKEY), state.u32_at(8))
        };
        assert_eq!(frame_and_state("2026", (1920, 1080)), (1920, 1080, STATE_NATIVE | STATE_FKEY, 1));
        assert_eq!(frame_and_state("2026", (3840, 2160)), (1920, 1080, STATE_NATIVE | STATE_FKEY, 2), "4K: 2x2 pixels");
        assert_eq!(frame_and_state("2026", (5120, 2880)), (1706, 960, STATE_NATIVE | STATE_FKEY, 3), "5K: 3x3");
        assert_eq!(frame_and_state("2026", (1300, 700)), (1300, 700, STATE_NATIVE | STATE_FKEY, 1), "any aspect");
        assert_eq!(frame_and_state("classic", (1920, 1080)), (960, 600, 0, 0), "the mode, in the 4:3 box");
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

    #[test]
    fn the_same_sample_is_sent_once() {
        let mut sys = Sys::new(Vec::new());
        let wav = b"RIFF....WAVE".to_vec();
        assert_eq!(sys.sample(&wav).unwrap(), 0);
        assert_eq!(sys.sample(&wav).unwrap(), 0);
        assert_eq!(sys.sample(b"RIFF2").unwrap(), 1);
        let recs = Record::split(&sys.out);
        assert_eq!(recs.iter().filter(|r| r.kind == Record::SAMPLE).count(), 2);
    }
}
