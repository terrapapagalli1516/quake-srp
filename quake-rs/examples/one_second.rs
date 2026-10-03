//! One second of E1M1, with no platform: the engine as a library.
//!
//! It starts a game on e1m1, walks forward firing the shotgun for 72 host
//! frames, and writes what the player saw last (a PPM) and heard (a WAV). This is the loop every
//! platform runs — the browser's `quake-wasm` and `quaketool play` included:
//! the game hands back an 8-bit frame, the palette to show it through, and
//! sound calls; the platform presents the one and mixes the other.
//!
//! ```sh
//! cargo run --release --example one_second -- ../quake-data/ID1/PAK0.PAK target/e1m1.ppm target/e1m1.wav
//! ```

#![forbid(unsafe_code)]

use std::error::Error;
use std::rc::Rc;

use quake_rs::client::{cl_main, host_cmd, Vid};
use quake_rs::pak::Pak;
use quake_rs::qrand::QRand;
use quake_rs::render::{self, FramePalette, MipCvars, VideoCvars};
use quake_rs::snd::{Fixes, Mixer};

/// Id's host frame at its 72 fps cap.
const FRAME: f64 = 1.0 / 72.0;
/// The sound device's rate, and how far ahead of it the mixer may paint.
const RATE: u32 = 11025;
const BUFFER_PAIRS: usize = 1 << 14;

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();
    let [_, pak_path, ppm_path, wav_path] = &args[..] else {
        return Err("usage: one_second <pak0.pak> <out.ppm> <out.wav>".into());
    };

    // The data: id's pak, and the palette every frame is shown through.
    let pak = Pak::from_bytes("pak0.pak".into(), std::fs::read(pak_path)?)?;
    let palette_lmp = pak.read_file("gfx/palette.lmp")?.ok_or("no gfx/palette.lmp")?;
    let base_palette = render::parse_palette(&palette_lmp).ok_or("gfx/palette.lmp is short")?;
    let gamma = render::build_gamma_table(1.0);

    // A game on e1m1: server, client and renderer, owned by one value.
    let mut sound = Vec::new();
    let mut walk = host_cmd::build_walk_map(
        pak.clone(),
        "maps/e1m1.bsp",
        &Rc::new(QRand::new()),
        &mut sound,
        quake_rs::vm::MAX_EDICTS,
    )
    .ok_or("maps/e1m1.bsp would not load")?;
    let mut mixer = Mixer::new(&pak, RATE, Fixes::NONE);
    mixer.run(&pak, &sound);

    // The screen the platform offers: 640x400 shown at 4:3, id's video settings.
    let vid = Vid {
        width: 640,
        height: 400,
        display_aspect: 4.0 / 3.0,
        persp_span: render::PerspSpan::Spans16,
        video: VideoCvars::CLASSIC,
        mip: MipCvars::DEFAULT,
    };

    let mut pcm: Vec<i16> = Vec::new();
    let mut last = None;
    for n in 1..=72 {
        walk.in_fwd = 1.0; // "forward" and "fire" held, as keys or a pad would
        walk.in_attack = true;
        let frame = cl_main::walk_frame(&mut walk, FRAME, false, &vid);

        // Sound: the frame's calls into id's mixer, then paint up to "now".
        mixer.run(&pak, &frame.sound);
        let now = (f64::from(n) * FRAME * f64::from(RATE)) as i64;
        let pairs = mixer.samples_ahead(now, BUFFER_PAIRS);
        let at = pcm.len();
        pcm.resize(at + 2 * pairs, 0);
        mixer.paint(&mut pcm[at..]);

        last = Some(frame);
    }
    let frame = last.ok_or("no frame")?;

    // Picture: the 8-bit frame through its palette (id's VID_SetPalette: the
    // base palette, this frame's colour shifts, then gamma).
    let palette = FramePalette::new(&base_palette, &frame.cshifts, &gamma);
    let mut rgba = Vec::new();
    render::pack_rgba(&frame.image, &palette, &mut rgba, 1);
    let mut ppm = format!("P6\n{} {}\n255\n", frame.image.w, frame.image.h).into_bytes();
    for px in rgba.chunks_exact(4) {
        ppm.extend_from_slice(&px[..3]);
    }
    std::fs::write(ppm_path, ppm)?;
    std::fs::write(wav_path, wav(&pcm, RATE))?;
    println!(
        "{}x{} frame -> {ppm_path}; {:.2} s of sound -> {wav_path}",
        frame.image.w,
        frame.image.h,
        pcm.len() as f64 / 2.0 / f64::from(RATE)
    );
    Ok(())
}

/// A 16-bit stereo WAV file around `pcm`.
fn wav(pcm: &[i16], rate: u32) -> Vec<u8> {
    let data_len = u32::try_from(pcm.len() * 2).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(44 + pcm.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&2u16.to_le_bytes()); // stereo
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 4).to_le_bytes()); // bytes a second
    out.extend_from_slice(&4u16.to_le_bytes()); // bytes a frame
    out.extend_from_slice(&16u16.to_le_bytes()); // bits a sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}
