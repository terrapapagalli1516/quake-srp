//! The sound commands: the engine's mixer ([`quake_rs::snd::Mixer`]) run
//! natively.
//!
//! - `quaketool sound <pak> <demo> <out.wav> [--rate HZ] [--classic] [--fps F] [--trace FILE]`
//!   plays a recorded demo through the client and writes what the mixer
//!   paints to a 16-bit stereo `.wav`, so anyone can listen (`--trace`: every
//!   sounding channel after each frame). The host runs a
//!   frame every 1/F s (72 by default; Classic through `Host_FilterTime`'s
//!   gate) and after each one mixes ahead of a device clock that plays
//!   `rate` pairs a second, as `S_Update_` mixed ahead of the DMA position.
//!   Classic is id's mixer at id's 11025 Hz; the default is the 2026 mixer
//!   (every [`Fixes`]) at 48000 Hz.
//! - `quaketool sndscript <pak> <script> <out.raw> --rate HZ [--fixes] [--trace FILE]`
//!   runs one of the sound oracle's scripts (`oracle/sound.py`,
//!   `oracle/c/snd_oracle.c`) through the port: the same calls, the same
//!   device clock, raw 16-bit stereo out, and the same per-`update` trace.
//!   Without `--fixes` the mixer is Classic, which must match id's C sample
//!   for sample.

use std::fmt::Write as _;

use quake_rs::bsp::NUM_AMBIENTS;
use quake_rs::client::host::{host_filter_time, host_filter_time_uncapped};
use quake_rs::client::{Listener, SoundCall, Vid, cl_demo};
use quake_rs::pak::Pak;
use quake_rs::render;
use quake_rs::server::{SoundEvent, StaticSound};
use quake_rs::snd::{Fixes, Mixer, SoundMode};

/// The device ring's size in sample pairs (the oracle's fake DMA buffer):
/// the most `S_Update_` mixes ahead.
const RING_PAIRS: usize = 32768;

fn open_pak(path: &str) -> Result<Pak, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    Pak::from_bytes("pak0.pak".into(), bytes).map_err(|e| e.to_string())
}

/// `quaketool sound`: see the module note.
pub fn cmd_sound(pak_path: &str, demo: &str, out_path: &str, rest: &[String]) -> Result<String, String> {
    let (mut rate, mut classic, mut fps, mut trace_path) = (None, false, 72.0f64, None);
    let mut i = 0;
    while i < rest.len() {
        let val = |i: usize| rest.get(i + 1).ok_or_else(|| format!("{} needs a value", rest[i]));
        match rest[i].as_str() {
            "--rate" => {
                rate = Some(val(i)?.parse::<u32>().map_err(|_| "--rate HZ")?);
                i += 1;
            }
            "--trace" => {
                trace_path = Some(val(i)?.clone());
                i += 1;
            }
            "--fps" => {
                fps = val(i)?.parse::<f64>().map_err(|_| "--fps F")?;
                i += 1;
            }
            "--classic" => classic = true,
            a => return Err(format!("unknown argument {a:?}")),
        }
        i += 1;
    }
    let mode = if classic { SoundMode::Classic } else { SoundMode::Modern };

    let pak = open_pak(pak_path)?;
    let name = cl_demo::default_extension(demo, ".dem");
    let mut calls = Vec::new();
    let mut d = cl_demo::build_demo(pak.clone(), &name, &mut calls).ok_or_else(|| format!("{name}: couldn't open"))?;
    // `--rate` is the device's: Classic mixes at id's 11025 whatever it is
    // unless asked for another, the 2026 mixer at the device's.
    let mut mixer = match rate {
        Some(r) if classic => {
            let mut m = Mixer::new(&pak, r.clamp(1000, 192_000), Fixes::NONE);
            m.cvars.mixahead = mode.mixahead();
            m
        }
        _ => mode.mixer(&pak, rate.unwrap_or(48000).clamp(1000, 192_000)),
    };
    let rate = mixer.rate();
    mixer.run(&pak, &calls);
    let mut starts = count_starts(&calls);

    let vid = Vid { width: 320, height: 200, display_aspect: 4.0 / 3.0, exact_perspective: false, video: render::VideoCvars::CLASSIC, mip: render::MipCvars::DEFAULT };
    let (mut realtime, mut oldrealtime) = (0.0f64, 0.0f64);
    let mut pcm: Vec<i16> = Vec::new();
    let mut trace = String::new();
    let mut frames = 0u32;
    while !d.at_end() {
        realtime += 1.0 / fps;
        let frametime = if classic {
            let Some(t) = host_filter_time(realtime, &mut oldrealtime) else { continue };
            t
        } else {
            host_filter_time_uncapped(realtime, &mut oldrealtime)
        };
        let frame = cl_demo::demo_frame(&mut d, frametime as f32, false, &vid);
        render::recycle_image(frame.image);
        mixer.run(&pak, &frame.sound);
        starts += count_starts(&frame.sound);
        frames += 1;

        // S_Update_: mix ahead of the device, which has played `realtime`.
        let soundtime = (realtime * f64::from(rate)) as i64;
        let n = mixer.samples_ahead(soundtime, RING_PAIRS);
        let at = pcm.len();
        pcm.resize(at + 2 * n, 0);
        mixer.paint(&mut pcm[at..]);
        if trace_path.is_some() {
            write_trace(&mut trace, &mixer, realtime);
        }
    }
    std::fs::write(out_path, wav_bytes(&pcm, rate)).map_err(|e| format!("cannot write {out_path}: {e}"))?;
    if let Some(path) = &trace_path {
        std::fs::write(path, trace).map_err(|e| format!("cannot write {path}: {e}"))?;
    }

    let peak = pcm.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
    let clipped = pcm.iter().filter(|&&s| s == i16::MAX || s == i16::MIN).count();
    let rms = (pcm.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / pcm.len().max(1) as f64).sqrt();
    let mut o = String::new();
    let _ = writeln!(
        o,
        "{name}: {frames} frames, {starts} S_StartSound; {} at {rate} Hz ({}), {:.2} s: peak {peak}, rms {rms:.0}, {clipped} samples clipped -> {out_path}",
        if classic { "Classic" } else { "2026 mixer" },
        if classic { "id's mixer" } else { "every fix" },
        pcm.len() as f64 / 2.0 / f64::from(rate),
    );
    Ok(o)
}

/// The channels sounding after a frame, one line each.
fn write_trace(t: &mut String, mixer: &Mixer, realtime: f64) {
    let _ = writeln!(t, "t {realtime:.4} paintedtime {}", mixer.painted_time());
    for c in mixer.channels().filter(|c| c.leftvol > 0 || c.rightvol > 0) {
        let _ = writeln!(
            t,
            "  ch {} {} left {} right {} master {} pos {} ent {} chan {}",
            c.index, c.sample, c.leftvol, c.rightvol, c.master_vol, c.pos, c.entnum, c.entchannel
        );
    }
}

fn count_starts(calls: &[SoundCall]) -> usize {
    calls.iter().map(|c| if let SoundCall::Start { events, .. } = c { events.len() } else { 0 }).sum()
}

/// A 16-bit stereo PCM `.wav` of interleaved `pcm` at `rate`.
fn wav_bytes(pcm: &[i16], rate: u32) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + pcm.len() * 2);
    b.extend(b"RIFF");
    b.extend((36 + data_len).to_le_bytes());
    b.extend(b"WAVEfmt ");
    b.extend(16u32.to_le_bytes());
    b.extend(1u16.to_le_bytes()); // PCM
    b.extend(2u16.to_le_bytes()); // stereo
    b.extend(rate.to_le_bytes());
    b.extend((rate * 4).to_le_bytes());
    b.extend(4u16.to_le_bytes());
    b.extend(16u16.to_le_bytes());
    b.extend(b"data");
    b.extend(data_len.to_le_bytes());
    for s in pcm {
        b.extend(s.to_le_bytes());
    }
    b
}

// ---------------------------------------------------------------------------
// sndscript: the sound oracle's scripts, through the port
// ---------------------------------------------------------------------------

/// The script's state between lines: what `snd_oracle.c` keeps in `cl`,
/// its listener and its fake DMA clock.
struct Script {
    pak: Pak,
    mixer: Mixer,
    listener: Listener,
    leaf: Option<[u8; NUM_AMBIENTS]>,
    frametime: f64,
    soundtime: i64,
    pcm: Vec<i16>,
    trace: Option<String>,
}

/// `quaketool sndscript`: see the module note.
pub fn cmd_sndscript(pak_path: &str, script_path: &str, out_path: &str, rest: &[String]) -> Result<String, String> {
    let (mut rate, mut fixes, mut trace_path) = (None, Fixes::NONE, None);
    let mut i = 0;
    while i < rest.len() {
        let val = |i: usize| rest.get(i + 1).ok_or_else(|| format!("{} needs a value", rest[i]));
        match rest[i].as_str() {
            "--rate" => {
                rate = Some(val(i)?.parse::<u32>().map_err(|_| "--rate HZ")?);
                i += 1;
            }
            "--trace" => {
                trace_path = Some(val(i)?.clone());
                i += 1;
            }
            "--fixes" => fixes = Fixes::ALL,
            a => return Err(format!("unknown argument {a:?}")),
        }
        i += 1;
    }
    let rate = rate.ok_or("--rate HZ is required")?;
    let pak = open_pak(pak_path)?;
    let text = std::fs::read_to_string(script_path).map_err(|e| format!("cannot read {script_path}: {e}"))?;
    let mixer = Mixer::new(&pak, rate, fixes);
    let mut s = Script {
        pak,
        mixer,
        listener: Listener::zero(),
        leaf: Some([0; NUM_AMBIENTS]),
        frametime: 0.0,
        soundtime: 0,
        pcm: Vec::new(),
        trace: trace_path.as_ref().map(|_| String::new()),
    };
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("");
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }
        s.line(&words).map_err(|e| format!("{script_path}:{}: {e}: {line}", n + 1))?;
    }
    let bytes: Vec<u8> = s.pcm.iter().flat_map(|v| v.to_le_bytes()).collect();
    std::fs::write(out_path, bytes).map_err(|e| format!("cannot write {out_path}: {e}"))?;
    if let (Some(path), Some(trace)) = (trace_path, s.trace) {
        std::fs::write(&path, trace).map_err(|e| format!("cannot write {path}: {e}"))?;
    }
    Ok(String::new())
}

fn num<T: std::str::FromStr>(w: &[&str], i: usize) -> Result<T, String> {
    w.get(i).and_then(|v| v.parse().ok()).ok_or_else(|| format!("argument {i} missing or bad"))
}

fn vec3(w: &[&str], i: usize) -> Result<[f32; 3], String> {
    Ok([num(w, i)?, num(w, i + 1)?, num(w, i + 2)?])
}

impl Script {
    /// One script line; the commands are `snd_oracle.c`'s.
    fn line(&mut self, w: &[&str]) -> Result<(), String> {
        match w[0] {
            "cvar" => self.cvar(w.get(1).copied().unwrap_or(""), num(w, 2)?)?,
            "viewent" => self.mixer.set_view_entity(num(w, 1)?),
            "listener" => {
                self.listener = Listener { pos: vec3(w, 1)?, forward: vec3(w, 4)?, right: vec3(w, 7)? };
            }
            "leaf" if w.get(1) == Some(&"none") => self.leaf = None,
            "leaf" => self.leaf = Some([num(w, 1)?, num(w, 2)?, num(w, 3)?, num(w, 4)?]),
            "frametime" => self.frametime = num(w, 1)?,
            "start" => {
                // CL_ParseStartSoundPacket: volume byte / 255.0, attenuation byte / 64.0.
                let ev = SoundEvent {
                    entity: num(w, 1)?,
                    channel: num(w, 2)?,
                    sound_index: -1,
                    sample: w.get(3).ok_or("no sample")?.to_string(),
                    origin: vec3(w, 4)?,
                    volume: (f64::from(num::<i32>(w, 7)?) / 255.0) as f32,
                    attenuation: (f64::from(num::<i32>(w, 8)?) / 64.0) as f32,
                };
                self.mixer.start_sound(&self.pak, &ev);
            }
            "static" => {
                let st = StaticSound {
                    origin: vec3(w, 2)?,
                    sound_index: -1,
                    sample: w.get(1).ok_or("no sample")?.to_string(),
                    volume: num::<u8>(w, 5)? as f32 / 255.0,
                    attenuation: num::<u8>(w, 6)? as f32 / 64.0,
                };
                self.mixer.static_sound(&self.pak, &st);
            }
            "stop" => self.mixer.stop_sound(num(w, 1)?, num(w, 2)?),
            "stopall" => self.mixer.stop_all_sounds(),
            "local" => self.mixer.local_sound(&self.pak, w.get(1).ok_or("no sample")?),
            "update" => self.update(),
            "advance" => self.soundtime += num::<i64>(w, 1)?,
            other => return Err(format!("unknown command {other:?}")),
        }
        Ok(())
    }

    fn cvar(&mut self, name: &str, value: f32) -> Result<(), String> {
        let c = &mut self.mixer.cvars;
        match name {
            "volume" => c.volume = value,
            "nosound" => c.nosound = value != 0.0,
            "loadas8bit" => c.loadas8bit = value != 0.0,
            "ambient_level" => c.ambient_level = value,
            "ambient_fade" => c.ambient_fade = value,
            "_snd_mixahead" => c.mixahead = value,
            _ => return Err(format!("unknown cvar {name:?}")),
        }
        Ok(())
    }

    /// `S_Update`, then `S_Update_` against the script's device clock, and
    /// the trace `snd_oracle.c` prints.
    fn update(&mut self) {
        self.mixer.update(&self.listener, self.leaf, self.frametime);
        let n = self.mixer.samples_ahead(self.soundtime, RING_PAIRS);
        let at = self.pcm.len();
        self.pcm.resize(at + 2 * n, 0);
        self.mixer.paint(&mut self.pcm[at..]);
        if let Some(t) = &mut self.trace {
            let _ = writeln!(
                t,
                "update paintedtime {} total_channels {}",
                self.mixer.painted_time(),
                self.mixer.total_channels()
            );
            for c in self.mixer.channels() {
                let _ = writeln!(
                    t,
                    "  ch {} {} left {} right {} master {} pos {} end {} ent {} chan {}",
                    c.index, c.sample, c.leftvol, c.rightvol, c.master_vol, c.pos, c.end, c.entnum, c.entchannel
                );
            }
        }
    }
}
