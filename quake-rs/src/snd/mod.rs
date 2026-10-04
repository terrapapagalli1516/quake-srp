//! Sound: id's own mixer, ported from WinQuake's `snd_dma.c`, `snd_mix.c`
//! and `snd_mem.c`.
//!
//! | module | id's C | what |
//! |---|---|---|
//! | [`dma`] | `snd_dma.c` | the [`Mixer`]: the channel table, `S_StartSound` (`SND_PickChannel`, `SND_Spatialize`), `S_StaticSound`, `S_StopSound`, `S_Update` with the leaf ambients, `S_Update_`'s mix-ahead; the cvars and the [`Fixes`] to id's code |
//! | `mix` | `snd_mix.c` | [`Mixer::paint`]: `S_PaintChannels`, `SND_PaintChannelFrom8`/`16`, the scale table, `S_TransferStereo16` |
//! | [`mem`] | `snd_mem.c`, `snd_dma.c` | `GetWavinfo`, `S_LoadSound` + `ResampleSfx`, the `known_sfx` table |
//!
//! The mixer takes what the client says to the sound layer (the
//! [`SoundCall`](crate::client::SoundCall)s of a frame) and gives 16-bit
//! stereo PCM at the platform's rate: the browser's worker paints it into
//! the ring its page's AudioWorklet plays (quake-wasm `snd_dma`), and
//! `quaketool sound` renders a demo through it to a `.wav`;
//! `oracle/sound.py` checks it against id's C.

pub mod dma;
pub mod mem;
mod mix;

pub use dma::{
    AMBIENT_FADE_DEFAULT, AMBIENT_LEVEL_DEFAULT, AMBIENT_SAMPLES, AMBIENT_SKY, AMBIENT_WATER, ChannelState, Fixes,
    ID_RATE, MAX_CHANNELS, MAX_DYNAMIC_CHANNELS, Mixer, SLOP_MIXAHEAD, SoundCvars, SoundMode,
};
pub use mem::{LoadOptions, SfxCache, SfxData, WavInfo, load_sound, wav_info};
