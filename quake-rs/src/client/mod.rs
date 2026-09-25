//! The game client — the part of Quake that in id's source is `cl_*.c`,
//! `view.c` and the client half of `host.c`/`host_cmd.c`: it turns the local
//! server's state (or a recorded demo's stream) into a finished screen and
//! the calls a platform makes into its sound, video and console layers.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//!
//! ## Layout
//!
//! | module   | id counterpart | what |
//! |----------|----------------|------|
//! | [`cl_input`] | cl_input.c   | `KeyMove`: `CL_BaseMove`/`CL_AdjustAngles` over the held keys and bindings, the `cl_*` move cvars |
//! | [`cl_tent`] | cl_tent.c, r_part.c | temp-entity effects (explosions, impacts, their sounds), the model-flag trails |
//! | [`host`] | host.c | `Host_FilterTime`: the 72 fps gate and the frame time it hands the game |
//! | [`view`] | view.c         | `V_ParseDamage`, the damage kick, `V_BonusFlash_f`, the item get-times |

pub mod cl_input;
pub mod cl_tent;
pub mod host;
pub mod view;
