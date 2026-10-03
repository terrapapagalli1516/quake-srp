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
use crate::config::{exec_config, write_if_changed};
use crate::host::step;
use crate::input::{clear_all_states, gamepad, key_event, mouse_move, pointer_unlocked};
use crate::proto::{
    read_event, AudioCounts, Event, Msg, PCM_CLEAR, STATE_ASK, STATE_BIND_GRAB, STATE_CONSOLE,
    STATE_ALT_ENTER, STATE_MENU, STATE_NATIVE, STATE_PAUSED, STATE_TIMEDEMO, STATE_TOUCH, STATE_WALK,
};
use crate::savegame::scan_saves;
use crate::snd_dma::Audio;

/// Run the program: `quake.rc`'s startup (with the command line's `+`
/// commands, `command_line` being the arguments after the program's name),
/// then turns until the host closes the input.
pub(crate) fn run(mut input: impl Read, output: impl Write, command_line: &[String]) -> io::Result<()> {
    let mut sys = Sys::new(output);
    sys.host_init(command_line);
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
    config: Option<String>,
    /// When the last frame started: a timedemo's frames time themselves.
    last_frame: Instant,
    /// The UI state the page last heard (`State`).
    state: UiState,
    /// The CD's state the page last heard (`Cd`).
    cd_sent: Option<quake_rs::cd_audio::CdState>,
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
            state: (u32::MAX, 0, 0),
            cd_sent: None,
        }
    }

    /// `Host_Init`'s `quake.rc`: `exec config.cfg`, `stuffcmds` (the command
    /// line's `+` commands: the page's `?classic` is `+profile classic`), then
    /// `startdemos demo1 demo2 demo3` (the attract loop; a key brings up the
    /// menu). The Load/Save listings are read once here too. `CDAudio_Init`
    /// comes first: the disc is the tracks the command line names
    /// (`-cdtracks`, the page's music files).
    fn host_init(&mut self, command_line: &[String]) {
        self.audio.set_disc(quake_rs::cd_audio::Disc::from_args(command_line));
        // `config.cfg` as it stands after the exec, so the first frame writes
        // the file only when something since has changed a setting — the
        // command line's `profile` included, which then sticks, as a choice
        // made in the menu does.
        crate::app::ensure_app(|_| {}); // the settings exist before quake.rc runs
        self.config = exec_config().or_else(crate::config::current_text);
        crate::host_cmd::execute_console_command(&quake_rs::cmd::stuff_cmds(command_line));
        boot_attract();
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

    /// Did the last key or call just decide to quit ([`App::request_quit`]
    /// through [`crate::app::take_quit`])? If so, this is `Sys_Quit`'s
    /// moment: write the `Quit` record (id's end screen, if the pak has it)
    /// and close the turn one last time, so the host hears about it before
    /// the program ends. True when it did — the caller's turn is over, and
    /// so is [`run`]'s loop (id's `exit(0)`).
    fn maybe_quit(&mut self) -> io::Result<bool> {
        let Some(registered) = crate::app::take_quit() else { return Ok(false) };
        let screen = crate::common::pak().and_then(|pak| end_screen(&pak, registered));
        Msg::Quit { registered, screen: screen.as_deref() }.write_to(&mut self.out)?;
        self.end_turn(false)?;
        Ok(true)
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
                    if self.maybe_quit()? {
                        return Ok(None);
                    }
                    if ui_state() != self.state {
                        self.end_turn(!polling)?;
                    }
                }
                Event::Mouse { dx, dy } => mouse_move(dx, dy),
                Event::ClearKeys => clear_all_states(),
                Event::PointerUnlocked => pointer_unlocked(),
                Event::AudioReady { running, rate } => self.audio.device(running, rate),
                Event::AudioClock(pos) => self.audio.clock(pos),
                Event::AudioWake(pos) => {
                    // S_ExtraUpdate: top the device's ring up between frames.
                    // No host frame ran, so this is not a sample for the
                    // 2026 mixer's adaptive lead ([`Audio::mix`]).
                    self.audio.clock(pos);
                    self.write_sound(0.0, None)?;
                    self.out.flush()?;
                }
                Event::Window { w, h, dpr } => crate::vid::set_window(w, h, dpr),
                Event::Present(format) => crate::app::ensure_app(|a| a.present.set_format(format)),
                Event::Gamepad(pad) => gamepad(pad),
                Event::Call { id, line } => {
                    // The sound device's own calls, then the game's.
                    let answer = match self.audio.cd_call(&line).or_else(|| self.audio.call(&line)) {
                        Some((value, text)) => automation::Answer { value, text },
                        None => automation::call(&line),
                    };
                    Msg::Reply { id, value: answer.value, text: &answer.text }.write_to(&mut self.out)?;
                    // A call can quit too (the automation calls run through
                    // the same menu/console paths a key does: `exec quit`,
                    // `menu_quit_yes`).
                    if self.maybe_quit()? {
                        return Ok(None);
                    }
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
        let t0 = Instant::now();
        self.last_frame = t0;
        // A verify script's way to hold this frame late on purpose
        // (`crate::bench::maybe_stall`): zero code without `--features
        // bench`. Before `step`, so the sleep counts in `host_elapsed`
        // below, exactly as a slow render would.
        crate::bench::maybe_stall();
        // The page's refresh time as the old `step(dt: f32)` export took it.
        let ran = step(dt as f32) != 0;
        // How long this host frame actually took: while it ran, the worker
        // could not mix, nor answer the page's AudioWake between ticks
        // either. The 2026 mixer's lead adapts to it ([`Audio::mix`]);
        // Classic's `_snd_mixahead` ignores it.
        let host_elapsed = t0.elapsed().as_secs_f64();
        // S_Update_ every tick, even one Host_FilterTime's 72 fps cap skipped
        // (id's S_ExtraUpdate mixed between frames too): the device's ring
        // stays fed. The samples go first, ahead of the frame's pixels.
        self.write_sound(dt, Some(host_elapsed))?;
        if !ran {
            // Nothing new to show.
            return Ok(());
        }
        self.write_picture()?;
        self.write_rumbles()?;
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

    /// `VID_Update`: the newest frame, as the page takes it ([`crate::present`]).
    fn write_picture(&mut self) -> io::Result<()> {
        let out = &mut self.out;
        APP.with(|c| match c.borrow().as_ref().and_then(|a| a.present.msg()) {
            Some(frame) => frame.write_to(out),
            None => Ok(()),
        })
    }

    /// The tick's sound ([`Audio::frame`]): its samples for the page's ring
    /// (`Pcm`), and the device's counts (`Audio`). `host_elapsed` is the
    /// wall-clock time the host frame just took to compute (`None` between
    /// ticks, an `AudioWake` that ran no frame).
    fn write_sound(&mut self, dt: f64, host_elapsed: Option<f64>) -> io::Result<()> {
        let Some(pcm) = self.audio.frame(dt, host_elapsed) else { return Ok(()) };
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
        Msg::Audio(counts).write_to(&mut self.out)?;
        // The CD's state when it changed: the page plays it beside the ring.
        let cd = self.audio.cd_state();
        if cd != self.cd_sent {
            self.cd_sent = cd;
            if let Some(cd) = cd {
                Msg::Cd(cd).write_to(&mut self.out)?;
            }
        }
        Ok(())
    }

    /// The frame's rumbles (the 2026 `joy_rumble`), for the pad or a phone.
    fn write_rumbles(&mut self) -> io::Result<()> {
        let (mut rumbles, mut pad) = (Vec::new(), false);
        crate::app::ensure_app(|a| (rumbles, pad) = (a.pad.take_rumbles(), a.pad.pad_read));
        rumbles.into_iter().try_for_each(|rumble| Msg::Rumble { rumble, pad }.write_to(&mut self.out))
    }
}

/// `Sys_Quit`'s end screen (sys_dos.c, ~570): `end2.bin` registered, else
/// `end1.bin` — id's `COM_LoadHunkFile`, so it is read through the same
/// search path as any other file (a player's own `pak1.pak` can carry its
/// own `end2.bin`). 80x25 of (character, attribute) VGA text-mode byte
/// pairs; the DOS build stamped its version into row 0's red strip before
/// drawing it (`sprintf(ver, " v%4.2f", VERSION)`, written to every other
/// byte from column 72 — [`quake_rs::console::CON_VERSION`] is the same
/// string `Draw_ConsoleBackground` stamps into the console background).
/// `None` without that lump, or one that isn't exactly 4000 bytes (a
/// modified install): the host still quits, just with no screen to draw.
/// Takes `pak` rather than reading [`crate::common::pak`] itself so a test
/// can hand it a synthetic one with its own `end2.bin`.
pub(crate) fn end_screen(pak: &quake_rs::pak::Pak, registered: bool) -> Option<Vec<u8>> {
    let name = if registered { "end2.bin" } else { "end1.bin" };
    let mut screen = pak.read_file(name).ok().flatten()?;
    if screen.len() != 4000 {
        return None;
    }
    let ver = format!(" v{}", quake_rs::console::CON_VERSION);
    for (i, b) in ver.bytes().enumerate() {
        screen[72 * 2 + i * 2] = b;
    }
    Some(screen)
}

/// What the page's own UI needs of the game (the `State` record): the
/// flags, the menu screen showing, and the pixel size of a native picture.
type UiState = (u32, i32, u32);

/// The [`UiState`] now.
fn ui_state() -> UiState {
    let (native, alt_enter, pixel_size) = APP.with(|c| {
        c.borrow().as_ref().map_or((false, false, 0), |a| {
            let native = crate::vid::native(a);
            let threads = crate::vid::render_threads(a);
            let pixel = a.window.filter(|_| native).map_or(0, |w| crate::vid::pixel_size(&a.settings.cvars, w, a.dpr, threads));
            (native, a.settings.cvars.alt_enter, pixel)
        })
    });
    let (touch, ask, paused) = APP.with(|c| {
        c.borrow().as_ref().map_or((false, false, false), |a| {
            let paused = a.mode == 0 && a.walk.as_ref().is_some_and(|w| w.server.paused);
            (a.settings.cvars.touch, a.menu.asks_yes_no(), paused)
        })
    });
    let flags = [
        (crate::menu::menu_visible() != 0, STATE_MENU),
        (crate::console::console_visible() != 0, STATE_CONSOLE),
        (crate::app::in_walk_mode() != 0, STATE_WALK),
        (crate::menu::menu_bind_grabbing() != 0, STATE_BIND_GRAB),
        (timedemo_running() != 0, STATE_TIMEDEMO),
        (native, STATE_NATIVE),
        (alt_enter, STATE_ALT_ENTER),
        (touch, STATE_TOUCH),
        (ask, STATE_ASK),
        (paused, STATE_PAUSED),
    ]
    .iter()
    .filter(|(on, _)| *on)
    .fold(0, |f, (_, bit)| f | bit);
    (flags, crate::menu::menu_screen_id(), pixel_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{encode, Record, STATE_ASK, STATE_ALT_ENTER, STATE_NATIVE, STATE_PAUSED, STATE_TOUCH};

    /// quake.rc's `stuffcmds`: the command line's `+profile 2026` (the
    /// page's `?2026`; the tests start in Classic) runs after `config.cfg`,
    /// and sticks — the first frame writes it. Without it nothing is written.
    #[test]
    fn the_command_line_runs_after_config_cfg_and_sticks() {
        let cfg = || crate::common::read_file(crate::config::CONFIG_CFG).ok();
        run(encode::tick(1, 0.0).as_slice(), &mut Vec::new(), &[]).unwrap();
        assert_eq!(cfg(), None, "a first session with no change writes nothing");
        APP.with(|c| *c.borrow_mut() = None);
        let args = ["+profile".to_string(), "2026".to_string()];
        run(encode::tick(1, 0.0).as_slice(), &mut Vec::new(), &args).unwrap();
        APP.with(|c| assert_eq!(c.borrow().as_ref().unwrap().settings.profile, quake_rs::settings::Profile::Modern));
        let text = String::from_utf8(cfg().expect("written")).unwrap();
        assert_eq!(text, "// generated by quake, do not modify\nprofile \"2026\"\n");
    }

    /// Run the program on `input` and split what it wrote.
    fn run_on(input: &[u8]) -> Vec<Record> {
        let mut out = Vec::new();
        run(input, &mut out, &[]).expect("the program runs to the end of its input");
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
        // The attract demo sounds: every tick's samples — the tests' Classic
        // profile: id's mixer at 11025 Hz whatever the device — from the
        // device's position on (0.1 s ahead of it, then the ticks' worth),
        // the first clearing the ring (S_Init).
        let pcm: Vec<&Record> = recs.iter().filter(|r| r.kind == Record::PCM).collect();
        assert_eq!(pcm.len(), 3);
        let fields = |r: &Record| (r.u32_at(0), r.u32_at(4), r.u32_at(8), (r.payload.len() - 12) / 4);
        assert_eq!(fields(pcm[0]), (735, 11025, PCM_CLEAR, 1102));
        assert_eq!(fields(pcm[1]), (735 + 1102, 11025, 0, 735));
        assert_eq!(fields(pcm[2]), (735 + 1102 + 735, 11025, 0, 735));
        let audio = recs.iter().rfind(|r| r.kind == Record::AUDIO).expect("the device's counts");
        assert_eq!((audio.u32_at(0), audio.u32_at(4)), (11025, 0), "id's mixer in Classic");
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
        assert!(texts[0].starts_with("rate=11025 mode=classic "), "{}", texts[0]);
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
    /// the smallest pixel that keeps a 1080p frame's cost on one thread), and
    /// the `State` that tells the page to fill its box with that pixel size.
    /// Classic shows its video mode in the 4:3 box whatever the window.
    #[test]
    fn the_window_sets_a_native_picture_in_2026_and_nothing_in_classic() {
        let frame_and_state = |profile: &str, win: (u32, u32)| {
            let mut input = Vec::new();
            input.extend(encode::call(1, &format!("exec profile {profile}; r_threads 1")));
            input.extend(encode::window(win.0, win.1));
            input.extend(encode::tick(1, 0.0));
            input.extend(encode::tick(2, 0.0));
            let recs = run_on(&input);
            let frame = recs.iter().rev().find(|r| r.kind == Record::FRAME).expect("a frame");
            let (w, h) = (u16::from_le_bytes([frame.payload[0], frame.payload[1]]), u16::from_le_bytes([frame.payload[2], frame.payload[3]]));
            let state = recs.iter().rev().find(|r| r.kind == Record::STATE).unwrap();
            (w, h, state.u32_at(0) & (STATE_NATIVE | STATE_ALT_ENTER), state.u32_at(8))
        };
        assert_eq!(frame_and_state("2026", (1920, 1080)), (1920, 1080, STATE_NATIVE | STATE_ALT_ENTER, 1));
        assert_eq!(frame_and_state("2026", (3840, 2160)), (1920, 1080, STATE_NATIVE | STATE_ALT_ENTER, 2), "4K: 2x2 pixels");
        assert_eq!(frame_and_state("2026", (5120, 2880)), (1706, 960, STATE_NATIVE | STATE_ALT_ENTER, 3), "5K: 3x3");
        assert_eq!(frame_and_state("2026", (1300, 700)), (1300, 700, STATE_NATIVE | STATE_ALT_ENTER, 1), "any aspect");
        assert_eq!(frame_and_state("classic", (1920, 1080)), (960, 600, 0, 0), "the mode, in the 4:3 box");
    }

    /// The flags the page's touch controls read: `in_touch` (on in 2026),
    /// the menu waiting for y or n, the live game paused.
    #[test]
    fn the_state_says_touch_a_question_and_pause() {
        let flags_after = |lines: &[&str]| {
            let mut input = Vec::new();
            for (id, line) in (1..).zip(lines) {
                input.extend(encode::call(id, line));
            }
            input.extend(encode::tick(1, 0.0));
            let recs = run_on(&input);
            let state = recs.iter().rev().find(|r| r.kind == Record::STATE).unwrap();
            state.u32_at(0) & (STATE_TOUCH | STATE_ASK | STATE_PAUSED)
        };
        assert_eq!(flags_after(&["exec profile 2026"]), STATE_TOUCH);
        assert_eq!(flags_after(&["exec profile classic"]), 0);
        assert_eq!(flags_after(&["boot", "menu_cancel", "exec pause"]), STATE_PAUSED);
        assert_eq!(flags_after(&["exec pause"]), 0, "unpaused");
        let quit = ["boot", "menu_up", "menu_select"];
        assert_eq!(flags_after(&quit), STATE_ASK, "Main's last item, Quit, asks");
        assert_eq!(flags_after(&["menu_quit_no"]), 0);
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

    /// The page's `Gamepad` records reach `IN_Commands` in the next host
    /// frame: the 2026 pad's Start (`togglemenu`) opens the menu, and the
    /// frame's `State` says so.
    #[test]
    fn a_gamepad_record_is_read_at_the_next_frame() {
        use quake_rs::client::in_win::Pad;
        let start = Pad { standard: true, num_buttons: 17, pressed: 1 << 9, axes: [0.0; 6] };
        let mut input = Vec::new();
        input.extend(encode::call(1, "boot"));
        input.extend(encode::call(2, "menu_cancel"));
        input.extend(encode::call(3, "exec profile 2026"));
        input.extend(encode::gamepad(Some(start)));
        input.extend(encode::tick(1, 1.0 / 60.0));
        input.extend(encode::gamepad(None));
        input.extend(encode::tick(2, 1.0 / 60.0));
        let recs = run_on(&input);
        let states: Vec<u32> = recs.iter().filter(|r| r.kind == Record::STATE).map(|r| r.u32_at(0)).collect();
        assert_eq!(states[3] & STATE_MENU, 0, "no menu before the frame");
        assert_eq!(states.last().map(|s| s & STATE_MENU), Some(STATE_MENU), "Start opened it: {states:?}");
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

    /// Menu > Quit > Y ([`MenuAction::Quit`], `App::request_quit`): one
    /// `Quit` record — unregistered here, so `end1.bin` from the test pak,
    /// the DOS version stamped into row 0 — and the program ends right
    /// there: nothing answers the tick queued after it (`run`'s loop already
    /// returned, as `Sys_Quit`'s `exit(0)` ends id's process).
    #[test]
    fn menu_quit_yes_sends_the_end_screen_and_ends_the_program() {
        let mut input = Vec::new();
        input.extend(encode::call(1, "boot"));
        input.extend(encode::call(2, "menu_up")); // Main's last item is Quit
        input.extend(encode::call(3, "menu_select")); // raises the confirm prompt
        input.extend(encode::call(4, "menu_quit_yes"));
        input.extend(encode::tick(1, 1.0 / 60.0)); // never answered: the program is gone
        let recs = run_on(&input);
        let quits: Vec<&Record> = recs.iter().filter(|r| r.kind == Record::QUIT).collect();
        assert_eq!(quits.len(), 1, "{recs:?}");
        let q = quits[0];
        assert_eq!(q.payload[0], 0, "the test pak is unregistered: end1.bin");
        assert_eq!(q.payload.len(), 4 + 4000, "the whole end1.bin lump");
        let screen = &q.payload[4..];
        let pak = crate::common::pak().expect("the test pak");
        let expect = end_screen(&pak, false).expect("end1.bin is in the shareware pak");
        assert_eq!(screen, expect, "the same bytes `end_screen` reads and patches");
        // Row 0, column 72: " v1.09" in the char bytes, the attribute untouched.
        let ver: Vec<u8> = (0..6).map(|i| screen[72 * 2 + i * 2]).collect();
        assert_eq!(ver, b" v1.09");
        assert_eq!(screen[72 * 2 + 1], 0x48, "the attribute byte is id's, not overwritten");
        // The record order: the call's own Reply, then Quit, then the final
        // State + Sync `maybe_quit` closes the turn with.
        assert!(recs.len() >= 4, "{recs:?}");
        let tail = &recs[recs.len() - 4..];
        assert_eq!(
            [tail[0].kind, tail[1].kind, tail[2].kind, tail[3].kind],
            [Record::REPLY, Record::QUIT, Record::STATE, Record::SYNC],
        );
        assert_eq!(tail[3].payload[4], 0, "the last Sync does not wait: nothing follows");
        assert_eq!(tail[2].u32_at(0) & STATE_MENU, 0, "the menu closed (quit_yes also closes it)");
    }

    /// `quit` at the console (`key_dest == key_console`) quits at once, with
    /// no confirmation — `Host_Quit_f`'s immediate branch, same as id's.
    #[test]
    fn the_quit_command_from_the_console_skips_the_prompt() {
        let mut input = Vec::new();
        input.extend(encode::call(1, "boot"));
        input.extend(encode::call(2, "menu_cancel"));
        input.extend(encode::call(3, "console_toggle"));
        input.extend(encode::call(4, "exec quit"));
        let recs = run_on(&input);
        assert_eq!(recs.iter().filter(|r| r.kind == Record::QUIT).count(), 1, "{recs:?}");
    }

    /// `quit` bound to a key while playing (the console is not the
    /// keyboard's destination) raises the confirm prompt instead, exactly
    /// like Menu > Quit — it does not quit on the spot.
    #[test]
    fn the_quit_command_while_playing_asks_first() {
        let mut input = Vec::new();
        input.extend(encode::call(1, "boot"));
        input.extend(encode::call(2, "menu_cancel")); // key_dest = key_game
        input.extend(encode::call(3, "exec quit"));
        let recs = run_on(&input);
        assert_eq!(recs.iter().filter(|r| r.kind == Record::QUIT).count(), 0, "{recs:?}");
        let state = recs.iter().rev().find(|r| r.kind == Record::STATE).unwrap();
        assert_eq!(state.u32_at(0) & STATE_ASK, STATE_ASK, "the Quit prompt is up");
    }

    /// `end_screen` picks `end2.bin` registered, `end1.bin` not (`Sys_Quit`'s
    /// own choice), patches the version into row 0 either way without
    /// touching the attribute bytes, and is `None` for a missing or
    /// wrong-length lump (a modified install) — never a panic. A synthetic
    /// pak stands in for a player's own `pak1.pak` (the real shareware pak
    /// has no `end2.bin` to test against).
    #[test]
    fn end_screen_picks_the_right_file_and_patches_the_version() {
        let screen = |marker: u8| {
            let mut v = Vec::with_capacity(4000);
            for _ in 0..2000 {
                v.push(0x20); // a space, as most of id's screen is
                v.push(0x11); // a distinctive attribute, to prove it survives
            }
            v[0] = marker;
            v
        };
        let pak = crate::test_util::build_test_pak(&[("end1.bin", &screen(b'1')), ("end2.bin", &screen(b'2'))]);
        let shareware = end_screen(&pak, false).expect("end1.bin");
        assert_eq!(shareware[0], b'1', "unregistered reads end1.bin");
        let registered = end_screen(&pak, true).expect("end2.bin");
        assert_eq!(registered[0], b'2', "registered reads end2.bin");
        // " v1.09" (CON_VERSION) into row 0 col 72, the char bytes only
        // (every other byte): the attribute bytes in between are untouched.
        let ver: Vec<u8> = (0..6).map(|i| shareware[72 * 2 + i * 2]).collect();
        assert_eq!(ver, b" v1.09");
        assert_eq!(shareware[72 * 2 + 1], 0x11, "the attribute byte is id's, not overwritten");

        let short = crate::test_util::build_test_pak(&[("end1.bin", &[0u8; 10])]);
        assert_eq!(end_screen(&short, false), None, "a modified/short lump: no screen, not a panic");
        let missing = crate::test_util::build_test_pak(&[]);
        assert_eq!(end_screen(&missing, false), None, "no end1.bin at all");
    }
}
