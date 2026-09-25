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
//! | [`view`] | view.c         | `V_ParseDamage`, the damage kick, `V_BonusFlash_f`, the item get-times |

pub mod view;
