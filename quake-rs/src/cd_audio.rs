//! The CD player — what id's `cd_win.c` keeps of the drive: the track that
//! plays, whether it loops, paused or stopped, the `remap` table and the
//! `cd` command; the level from id's DOS `cd_audio.c`.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/cd_win.c` (`CDAudio_Play`, `CDAudio_Stop`,
//! `CDAudio_Pause`, `CDAudio_Resume`, `CD_f`, `CDAudio_MessageHandler`'s
//! `MCI_NOTIFY_SUCCESSFUL`, `CDAudio_Init`) and `WinQuake/cd_audio.c`
//! (`CDAudio_Update`'s volume).
//!
//! Quake's music was the CD's own audio tracks, played by the drive beside
//! the game's mix, never through it: the engine only told the drive which
//! track to play. It does here too. The client asks for a track where id's
//! did — `svc_cdtrack` at every level's signon (the worldspawn's `sounds`),
//! the QuakeC's intermission and finale tracks, a demo's forced track — and
//! pauses it with `svc_setpause`; [`CdAudio`] is the drive's state, and the
//! platform plays [`CdAudio::state`] (the browser: the player's own track
//! files, beside the AudioWorklet that plays the engine's mix). The disc
//! ([`Disc`]) is the tracks the player has.
//!
//! **The level.** WinQuake's MCI could not set a CD's volume, so its
//! `CDAudio_Update` made "CD Music Volume" a switch: any change snapped
//! `bgmvolume` to 0 (pausing the CD) or 1 (resuming it). id's DOS driver,
//! which Quake shipped as, set the drive's level from the slider
//! (`(int)(bgmvolume * 255)`, 0..255); the port does that, since it can.
//! Without a disc nothing here runs, as with id's `cd_null.c`, which is what
//! the C oracle is built with.

/// Track numbers run 1..=99 (Red Book); `remap` covers 0..100 as cd_win.c's.
pub const MAX_TRACKS: usize = 100;

/// A call into the CD player, where id's client made it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CdCall {
    /// `CDAudio_Play (track, looping)`: `svc_cdtrack` (looping), or the
    /// `cd play` / `cd loop` command.
    Play { track: u8, looping: bool },
    /// `CDAudio_Pause`: `svc_setpause` 1.
    Pause,
    /// `CDAudio_Resume`: `svc_setpause` 0.
    Resume,
}

impl CdCall {
    /// `CL_ParseServerMessage`'s `svc_cdtrack`: `CDAudio_Play` of the track
    /// (a demo's forced one while demos play), always looping.
    pub fn cdtrack(track: u8) -> CdCall {
        CdCall::Play { track, looping: true }
    }
}

/// The disc in the drive: which of its tracks are music. Track 1 of Quake's
/// CD is the game's data; the soundtrack is tracks 2..=11, which Steam and
/// GOG ship as files (`track02.ogg`…). A track the player has no file for
/// is, to the game, a track that is not audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disc {
    audio: [bool; MAX_TRACKS],
}

impl Disc {
    /// A disc whose music is `tracks` (each 1..=99; others are ignored).
    pub fn new(tracks: impl IntoIterator<Item = u8>) -> Disc {
        let mut audio = [false; MAX_TRACKS];
        for t in tracks {
            if (1..MAX_TRACKS).contains(&usize::from(t)) {
                audio[usize::from(t)] = true;
            }
        }
        Disc { audio }
    }

    /// The disc a command line names: `-cdtracks 2,3,5` (what the page
    /// passes for the track files it keeps). `None` without one, or with no
    /// track: no disc.
    pub fn from_args(args: &[String]) -> Option<Disc> {
        let list = args.iter().position(|a| a == "-cdtracks").and_then(|i| args.get(i + 1))?;
        let disc = Disc::new(list.split(',').filter_map(|t| t.trim().parse().ok()));
        (disc.max_track() > 0).then_some(disc)
    }

    /// `maxTrack`: the disc's last track (MCI's `MCI_STATUS_NUMBER_OF_TRACKS`).
    pub fn max_track(&self) -> u8 {
        self.audio.iter().rposition(|&a| a).map_or(0, |t| t as u8)
    }

    /// Whether `track` is music (`MCI_CDA_TRACK_AUDIO`).
    pub fn is_audio(&self, track: u8) -> bool {
        self.audio.get(usize::from(track)).copied().unwrap_or(false)
    }

    /// The music tracks, in order.
    pub fn tracks(&self) -> impl Iterator<Item = u8> + '_ {
        (1..MAX_TRACKS as u8).filter(|&t| self.is_audio(t))
    }
}

/// What the drive is doing: what the platform plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CdMode {
    /// Nothing plays (`CDAudio_Stop`, or never started).
    #[default]
    Stopped,
    /// `playing`: the track sounds.
    Playing,
    /// `CDAudio_Pause`: stopped where it was; `CDAudio_Resume` goes on
    /// from there.
    Paused,
}

/// The drive's state, for the platform: play [`CdState::track`] from its
/// start whenever [`CdState::serial`] changes (every `MCI_PLAY` from the
/// top), stop or go on where it was as [`CdState::mode`] says, at
/// [`CdState::volume`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CdState {
    /// Counts the starts of a track from its top.
    pub serial: u32,
    /// `playTrack` (0: none yet).
    pub track: u8,
    /// `playLooping`: the track starts again when it ends.
    pub looping: bool,
    pub mode: CdMode,
    /// The drive's level, 0..=1 (`cdvolume` / 255).
    pub volume: f32,
}

/// The CD player: cd_win.c's statics.
#[derive(Debug, Clone)]
pub struct CdAudio {
    /// The disc (`cdValid`: `None` after `cd eject`).
    disc: Option<Disc>,
    /// The disc as it was loaded, for `cd reset` (the drive re-reading it).
    loaded: Option<Disc>,
    /// `initialized`: a drive exists (a disc at startup). Without one every
    /// entry point returns at once, as `cd_null.c`'s.
    initialized: bool,
    /// `enabled` (`cd on` / `cd off`).
    enabled: bool,
    playing: bool,
    was_playing: bool,
    looping: bool,
    track: u8,
    remap: [u8; MAX_TRACKS],
    /// `cdvolume`, DOS's 0..=255.
    volume: u8,
    serial: u32,
}

impl CdAudio {
    /// `CDAudio_Init`: a drive with `disc` in it, or none at all (`None`:
    /// no music, `cd_null.c`). The drive starts at full level, as DOS's
    /// `CDAudio_SetVolume (255)`.
    pub fn new(disc: Option<Disc>) -> CdAudio {
        let mut remap = [0u8; MAX_TRACKS];
        for (n, r) in remap.iter_mut().enumerate() {
            *r = n as u8;
        }
        CdAudio {
            initialized: disc.is_some(),
            loaded: disc.clone(),
            disc,
            enabled: true,
            playing: false,
            was_playing: false,
            looping: false,
            track: 0,
            remap,
            volume: 255,
            serial: 0,
        }
    }

    /// Whether there is a drive (the player added music).
    pub fn has_drive(&self) -> bool {
        self.initialized
    }

    /// The disc's music tracks (none without a disc).
    pub fn tracks(&self) -> Vec<u8> {
        self.disc.as_ref().map(|d| d.tracks().collect()).unwrap_or_default()
    }

    /// What the platform plays now.
    pub fn state(&self) -> CdState {
        let mode = if self.playing {
            CdMode::Playing
        } else if self.was_playing {
            CdMode::Paused
        } else {
            CdMode::Stopped
        };
        CdState {
            serial: self.serial,
            track: self.track,
            looping: self.looping,
            mode,
            volume: f32::from(self.volume) / 255.0,
        }
    }

    /// Carry out one of the client's calls; console lines go to `con`.
    pub fn call(&mut self, call: CdCall, con: &mut Vec<String>) {
        match call {
            CdCall::Play { track, looping } => self.play(track, looping, con),
            CdCall::Pause => self.pause(),
            CdCall::Resume => self.resume(),
        }
    }

    /// `CDAudio_Play`: `track` through `remap`; a track the disc does not
    /// have as music is refused ("Bad track number" is a developer line, so
    /// silent; "not audio" is printed), and the one playing goes on. The
    /// same track already playing goes on; another stops it and starts
    /// this one from its top.
    pub fn play(&mut self, track: u8, looping: bool, con: &mut Vec<String>) {
        if !self.initialized || !self.enabled {
            return;
        }
        let Some(disc) = &self.disc else { return };
        let track = self.remap.get(usize::from(track)).copied().unwrap_or(0);
        if track < 1 || track > disc.max_track() {
            return; // Con_DPrintf ("CDAudio: Bad track number %u.\n")
        }
        if !disc.is_audio(track) {
            con.push(format!("CDAudio: track {track} is not audio"));
            return;
        }
        if self.playing {
            if self.track == track {
                return;
            }
            self.stop();
        }
        // MCI_PLAY from the track's top.
        self.serial = self.serial.wrapping_add(1);
        self.looping = looping;
        self.track = track;
        self.playing = true;
    }

    /// `CDAudio_Stop`: nothing plays, and nothing to resume.
    pub fn stop(&mut self) {
        if !self.initialized || !self.enabled || !self.playing {
            return;
        }
        self.was_playing = false;
        self.playing = false;
    }

    /// `CDAudio_Pause`.
    pub fn pause(&mut self) {
        if !self.initialized || !self.enabled || !self.playing {
            return;
        }
        self.was_playing = self.playing;
        self.playing = false;
    }

    /// `CDAudio_Resume`: a paused track goes on where it stopped.
    pub fn resume(&mut self) {
        if !self.initialized || !self.enabled || self.disc.is_none() || !self.was_playing {
            return;
        }
        self.playing = true;
    }

    /// `CDAudio_MessageHandler`'s `MCI_NOTIFY_SUCCESSFUL`: the start numbered
    /// `serial` played to its end. A looping track starts again
    /// (`CDAudio_Play (playTrack, true)`, through `remap` once more, as id's);
    /// otherwise nothing plays. A report about an older start is stale.
    pub fn track_ended(&mut self, serial: u32, con: &mut Vec<String>) {
        if serial != self.serial || !self.playing {
            return;
        }
        self.playing = false;
        if self.looping {
            self.play(self.track, true, con);
        }
    }

    /// `CDAudio_Update`, DOS's (the module's doc says why): the drive's level
    /// follows `bgmvolume`, which is held to 0..=1 while there is a drive.
    pub fn update(&mut self, bgmvolume: &mut f32) {
        if !self.initialized || !self.enabled {
            return;
        }
        // (int)(bgmvolume.value * 255.0): C's truncation toward zero.
        let level = (*bgmvolume * 255.0) as i32;
        if level == i32::from(self.volume) {
            return;
        }
        self.volume = if level < 0 {
            *bgmvolume = 0.0;
            0
        } else if level > 255 {
            *bgmvolume = 1.0;
            255
        } else {
            level as u8
        };
    }

    /// `CD_f`, the `cd` command: `on`, `off`, `reset`, `remap`, `close`,
    /// `play`, `loop`, `stop`, `pause`, `resume`, `eject`, `info`. `argv[0]`
    /// is `cd`. Without a drive there is no such command in id's build; the
    /// port says why nothing plays.
    pub fn command(&mut self, argv: &[&str], con: &mut Vec<String>) {
        let Some(&command) = argv.get(1) else { return };
        if !self.initialized {
            con.push("No CD in player.".to_string());
            return;
        }
        let command = command.to_ascii_lowercase();
        match command.as_str() {
            "on" => self.enabled = true,
            "off" => {
                self.stop();
                self.enabled = false;
            }
            "reset" => {
                self.enabled = true;
                self.stop();
                for (n, r) in self.remap.iter_mut().enumerate() {
                    *r = n as u8;
                }
                self.disc = self.loaded.clone(); // CDAudio_GetAudioDiskInfo
            }
            "remap" => self.remap_command(&argv[2..], con),
            "close" => {} // CDAudio_CloseDoor
            _ if self.disc.is_none() => con.push("No CD in player.".to_string()),
            "play" | "loop" => {
                let track = crate::cvar::atof(argv.get(2).copied().unwrap_or("")) as i32;
                self.play(track as u8, command == "loop", con);
            }
            "stop" => self.stop(),
            "pause" => self.pause(),
            "resume" => self.resume(),
            "eject" => {
                self.stop();
                self.disc = None;
            }
            "info" => self.info(con),
            _ => {}
        }
    }

    /// `cd remap`: with no tracks, list those that are remapped; else track
    /// `n` plays `argv[n - 1]` (`Q_atoi`, as a byte).
    fn remap_command(&mut self, tracks: &[&str], con: &mut Vec<String>) {
        if tracks.is_empty() {
            for (n, &r) in self.remap.iter().enumerate().skip(1) {
                if usize::from(r) != n {
                    con.push(format!("  {n} -> {r}"));
                }
            }
            return;
        }
        for (n, t) in tracks.iter().enumerate().take(MAX_TRACKS - 1) {
            self.remap[n + 1] = crate::cvar::atof(t) as i32 as u8;
        }
    }

    /// `cd info`.
    fn info(&self, con: &mut Vec<String>) {
        let max = self.disc.as_ref().map_or(0, Disc::max_track);
        con.push(format!("{max} tracks"));
        let how = if self.looping { "looping" } else { "playing" };
        if self.playing {
            con.push(format!("Currently {how} track {}", self.track));
        } else if self.was_playing {
            con.push(format!("Paused {how} track {}", self.track));
        }
        con.push(format!("Volume is {}", self.volume));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive() -> CdAudio {
        CdAudio::new(Some(Disc::new(2..=11)))
    }

    fn play(cd: &mut CdAudio, track: u8) -> Vec<String> {
        let mut con = Vec::new();
        cd.call(CdCall::cdtrack(track), &mut con);
        con
    }

    #[test]
    fn a_level_track_plays_looping_and_the_same_track_goes_on() {
        let mut cd = drive();
        assert_eq!(cd.state().mode, CdMode::Stopped);
        play(&mut cd, 6);
        let s = cd.state();
        assert_eq!((s.track, s.looping, s.mode, s.serial), (6, true, CdMode::Playing, 1));
        // A restart's svc_cdtrack: the same track, not started again.
        play(&mut cd, 6);
        assert_eq!(cd.state().serial, 1);
        // The intermission's track 3 stops it and starts 3 from its top.
        play(&mut cd, 3);
        assert_eq!((cd.state().track, cd.state().serial), (3, 2));
    }

    #[test]
    fn a_track_the_disc_lacks_leaves_the_music_playing() {
        let mut cd = CdAudio::new(Some(Disc::new([2, 3, 5])));
        play(&mut cd, 2);
        // Past the last track: Bad track number, a developer line.
        assert!(play(&mut cd, 9).is_empty());
        // The data track, and a hole the player has no file for.
        assert_eq!(play(&mut cd, 1), ["CDAudio: track 1 is not audio"]);
        assert_eq!(play(&mut cd, 4), ["CDAudio: track 4 is not audio"]);
        assert!(play(&mut cd, 0).is_empty(), "a map without `sounds`");
        assert_eq!((cd.state().track, cd.state().mode, cd.state().serial), (2, CdMode::Playing, 1));
    }

    #[test]
    fn pause_and_resume_go_on_where_it_was_and_stop_forgets() {
        let mut cd = drive();
        let mut con = Vec::new();
        play(&mut cd, 4);
        cd.call(CdCall::Pause, &mut con);
        assert_eq!(cd.state().mode, CdMode::Paused);
        cd.call(CdCall::Resume, &mut con);
        assert_eq!((cd.state().mode, cd.state().serial), (CdMode::Playing, 1), "resumed, not restarted");
        cd.stop();
        cd.call(CdCall::Resume, &mut con);
        assert_eq!(cd.state().mode, CdMode::Stopped, "nothing to resume after a stop");
        // Paused, a new level's track starts even when it is the same one
        // (CDAudio_Play only skips a track that is `playing`).
        play(&mut cd, 4);
        cd.call(CdCall::Pause, &mut con);
        play(&mut cd, 4);
        assert_eq!((cd.state().mode, cd.state().serial), (CdMode::Playing, 3));
    }

    #[test]
    fn a_track_that_ends_loops_or_stops() {
        let mut cd = drive();
        let mut con = Vec::new();
        cd.command(&["cd", "play", "5"], &mut con);
        assert_eq!((cd.state().track, cd.state().looping), (5, false));
        cd.track_ended(0, &mut con);
        assert_eq!(cd.state().mode, CdMode::Playing, "a stale report");
        cd.track_ended(1, &mut con);
        assert_eq!(cd.state().mode, CdMode::Stopped);
        // Stopped, the same track as a level's plays again (it is not `playing`).
        play(&mut cd, 5);
        assert_eq!((cd.state().mode, cd.state().serial), (CdMode::Playing, 2));
        cd.track_ended(2, &mut con);
        assert_eq!((cd.state().mode, cd.state().serial), (CdMode::Playing, 3), "looping: again from the top");
    }

    #[test]
    fn the_level_follows_bgmvolume_held_to_0_1() {
        let mut cd = drive();
        let mut bgm = 0.7;
        cd.update(&mut bgm);
        assert_eq!(cd.state().volume, 178.0 / 255.0, "(int)(0.7 * 255) = 178");
        bgm = 1.5;
        cd.update(&mut bgm);
        assert_eq!((bgm, cd.state().volume), (1.0, 1.0));
        bgm = -0.2;
        cd.update(&mut bgm);
        assert_eq!((bgm, cd.state().volume), (0.0, 0.0));
        // No drive: bgmvolume is nobody's business (cd_null.c).
        let mut none = CdAudio::new(None);
        bgm = 1.5;
        none.update(&mut bgm);
        assert_eq!(bgm, 1.5);
    }

    #[test]
    fn the_cd_command() {
        let mut cd = drive();
        let mut con = Vec::new();
        cd.command(&["cd", "loop", "7"], &mut con);
        cd.command(&["cd", "info"], &mut con);
        assert_eq!(con, ["11 tracks", "Currently looping track 7", "Volume is 255"]);
        con.clear();
        cd.command(&["cd", "pause"], &mut con);
        cd.command(&["cd", "info"], &mut con);
        assert_eq!(con[1], "Paused looping track 7");
        cd.command(&["cd", "remap", "2", "9"], &mut con);
        con.clear();
        cd.command(&["cd", "remap"], &mut con);
        assert_eq!(con, ["  1 -> 2", "  2 -> 9"]);
        con.clear();
        play(&mut cd, 2);
        assert_eq!(cd.state().track, 9, "track 2 plays 9");
        cd.command(&["cd", "off"], &mut con);
        assert_eq!(cd.state().mode, CdMode::Stopped);
        play(&mut cd, 3);
        assert_eq!(cd.state().mode, CdMode::Stopped, "off: nothing plays");
        cd.command(&["cd", "reset"], &mut con);
        play(&mut cd, 2);
        assert_eq!((cd.state().track, cd.state().mode), (2, CdMode::Playing), "reset: on, no remap");
        cd.command(&["cd", "eject"], &mut con);
        cd.command(&["cd", "play", "3"], &mut con);
        assert_eq!(con, ["No CD in player."]);
        con.clear();
        let mut none = CdAudio::new(None);
        none.command(&["cd", "info"], &mut con);
        assert_eq!(con, ["No CD in player."]);
        assert!(!none.has_drive() && none.tracks().is_empty());
    }

    #[test]
    fn the_page_names_the_disc_on_the_command_line() {
        let args = |s: &str| s.split(' ').map(str::to_string).collect::<Vec<_>>();
        let disc = Disc::from_args(&args("-basedir . -cdtracks 2,3,11 +profile classic")).unwrap();
        assert_eq!(disc.tracks().collect::<Vec<_>>(), [2, 3, 11]);
        assert_eq!(disc.max_track(), 11);
        assert!(Disc::from_args(&args("-basedir .")).is_none());
        assert!(Disc::from_args(&args("-cdtracks 0,200,x")).is_none());
    }
}
