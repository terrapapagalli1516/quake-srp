//! Sound: id's own mixer, ported from WinQuake's `snd_dma.c`, `snd_mix.c`
//! and `snd_mem.c`, and the older bridge for a platform that mixes for
//! itself.
//!
//! | module | id's C | what |
//! |---|---|---|
//! | [`dma`] | `snd_dma.c` | the [`Mixer`]: the channel table, `S_StartSound` (`SND_PickChannel`, `SND_Spatialize`), `S_StaticSound`, `S_StopSound`, `S_Update` with the leaf ambients, `S_Update_`'s mix-ahead; the cvars and the [`Fixes`] to id's code |
//! | `mix` | `snd_mix.c` | [`Mixer::paint`]: `S_PaintChannels`, `SND_PaintChannelFrom8`/`16`, the scale table, `S_TransferStereo16` |
//! | [`mem`] | `snd_mem.c`, `snd_dma.c` | `GetWavinfo`, `S_LoadSound` + `ResampleSfx`, the `known_sfx` table |
//! | [`webaudio`] | `snd_dma.c` | the page's bridge: sounds and their spatial parameters queued for Web Audio to mix, until the page plays the mixer's PCM |
//!
//! The mixer takes what the client says to the sound layer (the
//! [`SoundCall`](crate::client::SoundCall)s of a frame) and gives 16-bit
//! stereo PCM at the platform's rate. `quaketool sound` renders a demo
//! through it to a `.wav`; `oracle/sound.py` checks it against id's C.

pub mod dma;
pub mod mem;
mod mix;
pub mod webaudio;

pub use dma::{
    AMBIENT_FADE_DEFAULT, AMBIENT_LEVEL_DEFAULT, AMBIENT_SAMPLES, AMBIENT_SKY, AMBIENT_WATER, ChannelState, Fixes,
    ID_RATE, MAX_CHANNELS, MAX_DYNAMIC_CHANNELS, MODERN_MIXAHEAD, Mixer, SoundCvars, SoundMode,
};
pub use mem::{LoadOptions, SfxCache, SfxData, WavInfo, load_sound, wav_info};
pub use webaudio::{
    AmbientChannels, MAX_STATIC_SOUNDS, QUEUE_CAP, SndParams, StaticLoop, queue_sounds, queue_static_sounds,
};
