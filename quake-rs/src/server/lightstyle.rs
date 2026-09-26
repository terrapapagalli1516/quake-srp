//! Animated light styles: the `lightstyle()` builtin's pattern table and
//! `R_AnimateLight`'s per-style brightness scales.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources:
//! * `WinQuake/pr_cmds.c` — `PF_lightstyle` (the `sv.lightstyles[64]` store).
//! * `WinQuake/r_light.c` — `R_AnimateLight` (pattern letter →
//!   `d_lightstylevalue`), shared by the live server and demo playback.

use super::{Outbox, Server};
use crate::vm::Vm;
use crate::Result;

// ---------------------------------------------------------------------------
// Animated light styles (PF_lightstyle / R_AnimateLight).
//
// `lightstyle(style, val)` (pr_cmds.c PF_lightstyle, #35) stores a pattern
// string per style index into `sv.lightstyles[64]`. The QuakeC worldspawn calls
// it for styles 0..11 with the classic patterns (steady "m", torch flicker
// "mmnmmommommnonmmonqnmmo", slow pulse "abcdefghijklmnopqrstuvwxyz…", …), so the
// table is populated automatically while `spawn_entities` runs worldspawn.
//
// This is *persistent map state* — the renderer reads it every frame to animate
// lightmaps — so unlike the per-frame sound/particle/temp-entity queues it is
// OWNED by the [`Server`] (the `lightstyles` field), not drained-and-discarded.
// The builtin cannot reach the `Server`, only its outbox, so it sends the
// write there (the `svc_lightstyle` the C broadcast), and the server applies
// the writes to its table after each QuakeC execution window
// (`spawn_entities` / `run_frame`); the getters read the owned table.
// ---------------------------------------------------------------------------

/// `MAX_LIGHTSTYLES` (quakedef.h): the size of `sv.lightstyles[]`.
pub const MAX_LIGHTSTYLES: usize = 64;

/// `PF_lightstyle` (#35): `void(float style, string value) lightstyle`. The C
/// `PF_lightstyle` stored `value` into `sv.lightstyles[style]` and, for live
/// clients, broadcast an `svc_lightstyle` update. This headless server has no
/// netcode, so we only store the pattern (PARM0 = style index, PARM1 = the
/// pattern string). An out-of-range style index is ignored without panicking,
/// matching the C's silent clamp.
pub(super) fn bi_lightstyle(vm: &mut Vm) -> Result<()> {
    let style = vm.arg_float(0);
    let val = vm.arg_string(1);
    // The C truncates the float to an int index; negatives / NaN are out of
    // range and dropped.
    if style.is_finite() && style >= 0.0 && (style as usize) < MAX_LIGHTSTYLES {
        vm.with_host(|_, h| h.outbox().lightstyles.push((style as usize, val)));
    }
    Ok(())
}

impl Server {
    /// Apply the `lightstyle()` writes the QuakeC sent since the last call to
    /// the server's `sv.lightstyles` table (see the note above).
    pub(super) fn apply_lightstyles(&mut self) {
        for (style, val) in self.take_outbox(|o: &mut Outbox| &mut o.lightstyles) {
            self.lightstyles[style] = val;
        }
    }
}

/// `R_AnimateLight` letter scale: map a pattern character to its
/// `d_lightstylevalue` (the C `(c - 'a') * 22`). Non-letters fold modulo 26 onto
/// the `a..z` range like the C's byte arithmetic, never reading out of bounds.
fn lightstyle_letter_value(ch: u8) -> f32 {
    // The C indexes `lightstyles[j].map[k]` (an ASCII byte) and computes
    // `(map[k]-'a')*22`. Authored patterns are always `a..z`; for robustness we
    // wrap any other byte into `0..=25` rather than producing a wild value.
    let v = (ch.wrapping_sub(b'a')) % 26;
    v as f32 * 22.0
}

/// `R_AnimateLight` (r_light.c) over an arbitrary style table: the per-style
/// brightness scale at game `time`, one entry per [`MAX_LIGHTSTYLES`] index.
/// Shared by [`Server::lightstyle_scales`] (the live walk's `sv.lightstyles`)
/// and demo playback (the RECORDED `svc_lightstyle` table a `.dem` carries),
/// so both paths animate through the identical 10 Hz logic.
///
/// `styles` shorter than [`MAX_LIGHTSTYLES`] treats the missing tail as unset
/// (scale `1.0`), so a demo-frame table can be passed directly.
///
/// For style `j` with pattern string of length `L`:
/// * `L == 0` (unset) → scale `1.0` (the C `d_lightstylevalue = 256`, i.e.
///   "normal"). Treating a missing style as normal keeps faces that reference
///   an unset style at full brightness rather than going dark.
/// * else the string animates at 10 chars/sec: `k = floor(time*10) mod L`,
///   `ch = string[k]`, and the C `d_lightstylevalue[j] = (ch - 'a') * 22`
///   (so `'a'` → 0 = dark, `'m'` → 264 = normal, `'z'` → 550 ≈ double-bright).
pub fn lightstyle_scales_at(styles: &[String], time: f32) -> [f32; MAX_LIGHTSTYLES] {
    // Normalise by 256 — id's white point — NOT by 'm' (264). R_AnimateLight
    // sets d_lightstylevalue[j] = (letter-'a')*22 (so worldspawn's lightstyle
    // (0,"m") gives style 0 = 264), and R_BuildLightMap renders luxel*scale
    // against the constant 255*256 white point. So a steady 'm' world is
    // luxel*264/256 = 1.03125x — slightly brighter than a literal luxel*256.
    // Normalising by 'm' (264) made style 0 exactly 1.0, rendering the entire
    // static-lit world ~1 colormap row too dark; /256 matches id. An UNSET style
    // still maps to 1.0 below (R_AnimateLight's length==0 default of 256).
    const NORMAL: f32 = 256.0;
    // Animation phase in characters; floor(time*10), guarded against a
    // non-finite/huge time so the modulo index never overflows or panics.
    let phase: i64 = if time.is_finite() {
        (time * 10.0).floor() as i64
    } else {
        0
    };
    std::array::from_fn(|j| {
        let s = styles.get(j).map(|s| s.as_bytes()).unwrap_or(b"");
        if s.is_empty() {
            return 1.0; // unset style -> normal (256/264 ~ never; treat as 1.0)
        }
        let len = s.len() as i64;
        // Positive modulo: ((phase % len) + len) % len keeps k in 0..len even
        // for a negative phase (a time before 0).
        let k = (((phase % len) + len) % len) as usize;
        lightstyle_letter_value(s[k]) / NORMAL
    })
}

impl Server {
    /// The raw light-style pattern string at index `style`, or `""` for an unset
    /// or out-of-range index. (Mostly for inspection / tests; the renderer wants
    /// [`Self::lightstyle_scales`].)
    pub fn lightstyle(&self, style: usize) -> &str {
        self.lightstyles.get(style).map(String::as_str).unwrap_or("")
    }

    /// `R_AnimateLight` (r_light.c): the per-style brightness scale at game `time`,
    /// one entry per `MAX_LIGHTSTYLES` style index, ready to pass to
    /// [`crate::render::render_scene_ext`].
    ///
    /// For style `j` with pattern string of length `L`:
    /// * `L == 0` (unset) → scale `1.0` (the C `d_lightstylevalue = 256`, i.e.
    ///   "normal"). Treating a missing style as normal keeps faces that reference
    ///   an unset style at full brightness rather than going dark.
    /// * else the string animates at 10 chars/sec: `k = floor(time*10) mod L`,
    ///   `ch = string[k]`, and the C `d_lightstylevalue[j] = (ch - 'a') * 22`
    ///   (so `'a'` → 0 = dark, `'m'` → 264 = normal, `'z'` → 550 ≈ double-bright).
    ///
    /// The C renders `luxel * d_lightstylevalue` against a constant `255 * 256`
    /// white point. This renderer stores luxels in `0..=255` and applies a
    /// multiplicative factor, so we normalise the style value by id's `256` white
    /// point: `scale = (ch - 'a') * 22 / 256`. Then `'m'` → `264/256 = 1.03125`
    /// (exactly id's steady-world brightness — normalising by `'m'` itself made the
    /// whole static-lit world ~1 colormap row too dark). An UNSET style still maps
    /// to `1.0` (R_AnimateLight's `length == 0` default of 256).
    pub fn lightstyle_scales(&self, time: f32) -> [f32; MAX_LIGHTSTYLES] {
        // Delegates to the shared table-driven helper so demo playback (the
        // recorded svc_lightstyle table) animates through the IDENTICAL logic.
        lightstyle_scales_at(&self.lightstyles, time)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progs::{Progs, OFS_PARM0};
    use crate::server::testutil::*;
    // ------------------------------------------------ animated light styles (#35)

    /// Drive `bi_lightstyle(style, val)` directly: PARM0 = style float, PARM1 =
    /// the interned pattern string. Returns nothing; the write waits in the
    /// server's outbox until a frame applies it.
    fn call_lightstyle(server: &mut Server, style: f32, val: &str) {
        let s = server.vm.intern(val);
        server.vm.set_gf(OFS_PARM0, style);
        // PARM1 is a string_t (an int handle), at OFS_PARM0 + 3.
        server.vm.set_gi(OFS_PARM0 + 3, s);
        bi_lightstyle(&mut server.vm).expect("bi_lightstyle");
    }

    #[test]
    fn bi_lightstyle_stores_pattern_and_getter_reflects_it() {
        let (img, _gc, _gd) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(empty_bsp(), progs).expect("server");

        // Fresh server: all styles empty.
        assert_eq!(server.lightstyle(0), "");
        assert_eq!(server.lightstyle(3), "");

        // Store a steady style 0 and a torch flicker at slot 3.
        call_lightstyle(&mut server, 0.0, "m");
        call_lightstyle(&mut server, 3.0, "mmnmmommommnonmmonqnmmo");
        // A frame applies the writes to the owned table (the production path).
        server.run_frame(0.1).expect("frame");

        assert_eq!(server.lightstyle(0), "m");
        assert_eq!(server.lightstyle(3), "mmnmmommommnonmmonqnmmo");
    }

    #[test]
    fn lightstyle_scales_maps_letters_to_brightness() {
        let (img, _gc, _gd) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(empty_bsp(), progs).expect("server");

        // Letter value is (c-'a')*22, normalised by id's 256 white point (NOT 'm'):
        // 'a' -> 0 (dark), 'm' -> 264/256 = 1.03125 (id's steady-world brightness),
        // 'z' -> 550/256 ~ 2.148, and an UNSET style -> 1.0 (R_AnimateLight's 256
        // default, so untouched faces stay neutral).
        call_lightstyle(&mut server, 0.0, "a");
        call_lightstyle(&mut server, 1.0, "m");
        call_lightstyle(&mut server, 2.0, "z");
        // style 4 left empty.
        server.run_frame(0.1).expect("frame");

        let sc = server.lightstyle_scales(0.0);
        assert!((sc[0] - 0.0).abs() < 1e-6, "'a' -> 0.0, got {}", sc[0]);
        // 'm' = (12*22)/256 = 264/256 = 1.03125 (id renders the steady world here).
        assert!((sc[1] - (264.0 / 256.0)).abs() < 1e-6, "'m' -> 1.03125, got {}", sc[1]);
        // 'z' = (25*22)/256 = 550/256 ~ 2.1484.
        assert!((sc[2] - (550.0 / 256.0)).abs() < 1e-5, "'z' -> ~2.148, got {}", sc[2]);
        assert!((sc[4] - 1.0).abs() < 1e-6, "unset style -> 1.0 (normal), got {}", sc[4]);
    }

    #[test]
    fn lightstyle_scales_animate_at_ten_per_second_with_modulo() {
        let (img, _gc, _gd) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(empty_bsp(), progs).expect("server");

        // A two-char flicker: 'a' (dark) then 'z' (bright). At 10 chars/sec it
        // toggles every 0.1s.
        call_lightstyle(&mut server, 1.0, "az");
        server.run_frame(0.1).expect("frame");

        let s_t0 = server.lightstyle_scales(0.00); // k = floor(0)=0 -> 'a' -> 0.0
        let s_t1 = server.lightstyle_scales(0.10); // k = floor(1)=1 -> 'z' -> ~2.08
        let s_t2 = server.lightstyle_scales(0.20); // k = floor(2)=0 (mod 2) -> 'a'
        assert!((s_t0[1] - 0.0).abs() < 1e-6, "t=0 -> 'a' 0.0, got {}", s_t0[1]);
        assert!(s_t1[1] > 2.0, "t=0.1 -> 'z' ~2.08, got {}", s_t1[1]);
        // Cycling: the index wraps modulo the string length, so t=0.2 == t=0.0.
        assert!((s_t2[1] - s_t0[1]).abs() < 1e-6, "modulo cycle: t=0.2 == t=0.0");
        // Across time the same style yields DIFFERENT scales (animation).
        assert!((s_t0[1] - s_t1[1]).abs() > 1e-3, "style must animate over time");
    }

    #[test]
    fn bi_lightstyle_out_of_range_index_is_ignored_without_panic() {
        let (img, _gc, _gd) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut server = Server::new(empty_bsp(), progs).expect("server");

        // Index 64 (== MAX_LIGHTSTYLES) is out of range -> dropped, no panic.
        call_lightstyle(&mut server, MAX_LIGHTSTYLES as f32, "z");
        // A wild / negative / non-finite index is also clamped out, no panic.
        call_lightstyle(&mut server, -5.0, "z");
        call_lightstyle(&mut server, 1.0e30, "z");
        call_lightstyle(&mut server, f32::NAN, "z");
        server.run_frame(0.1).expect("frame");

        // Nothing was stored; every scale is the unset normal 1.0.
        let sc = server.lightstyle_scales(0.0);
        assert!(sc.iter().all(|&s| (s - 1.0).abs() < 1e-6), "no style stored");
        // The valid last in-range index (63) still works as a sanity anchor.
        call_lightstyle(&mut server, 63.0, "a");
        server.run_frame(0.1).expect("frame");
        assert!((server.lightstyle_scales(0.0)[63] - 0.0).abs() < 1e-6, "index 63 valid");
    }

    #[test]
    fn fresh_server_clears_stale_lightstyles() {
        // A pattern one server has not applied yet must not reach a freshly
        // built server.
        let (img, _gc, _gd) = changelevel_progs();
        let progs = Progs::parse(&img).expect("parse");
        let mut s1 = Server::new(empty_bsp(), progs).expect("server");
        call_lightstyle(&mut s1, 1.0, "z"); // waits in s1's outbox
        // A new server has its own outbox; its first frame applies nothing.
        let progs2 = Progs::parse(&img).expect("parse");
        let mut s2 = Server::new(empty_bsp(), progs2).expect("server");
        s2.run_frame(0.1).expect("frame");
        assert_eq!(s2.lightstyle(1), "", "stale style must not leak into a new server");
        assert!((s2.lightstyle_scales(0.0)[1] - 1.0).abs() < 1e-6, "new server style 1 normal");
    }
}
