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

/// How [`lightstyle_scales_at`] steps a style through its pattern
/// (`r_lerplightstyles`, DarkPlaces' name), read each frame from the video
/// cvars ([`crate::render::VideoCvars::lightstyles`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LerpLightStyles {
    /// id's `R_AnimateLight`: the letter of the current tenth of a second, so
    /// an animated light snaps to its next brightness ten times a second.
    #[default]
    Classic,
    /// The brightness glides from each letter to the next across its tenth,
    /// in steps of [`GLIDE_STEP`] of id's light units; at every whole tenth
    /// it is id's letter.
    Smooth,
}

/// The gliding light style's step, in id's integer light units
/// (`d_lightstylevalue`, 256 the white point): a letter is 22 units from
/// the next, so the glide crosses a one-letter step in 11 values and lands
/// on each letter exactly. The lit-surface caches are keyed on the value,
/// so every new value rebakes each block the style lights: in steps of 1,
/// a flickering torch's blocks rebake on half to three quarters of the
/// frames at 480 Hz; in steps of 2, about 40% fewer, and no difference to
/// see — a step is half a colormap row at the brightest luxel, where 4
/// would be a whole one (`FRAMERATE.md`, "Light styles between their
/// letters").
pub const GLIDE_STEP: i32 = 2;

/// `R_AnimateLight` letter scale: map a pattern character to its
/// `d_lightstylevalue` (the C `(c - 'a') * 22`). Non-letters fold modulo 26 onto
/// the `a..z` range like the C's byte arithmetic, never reading out of bounds.
fn lightstyle_letter_value(ch: u8) -> i32 {
    // The C indexes `lightstyles[j].map[k]` (an ASCII byte) and computes
    // `(map[k]-'a')*22`. Authored patterns are always `a..z`; for robustness we
    // wrap any other byte into `0..=25` rather than producing a wild value.
    let v = (ch.wrapping_sub(b'a')) % 26;
    i32::from(v) * 22
}

/// One style's `d_lightstylevalue` (256 is id's white point) for the pattern
/// `map` at animation phase `i + frac` — `i = floor(time*10)`, `frac` its
/// fraction in `0..=1`.
///
/// [`LerpLightStyles::Classic`] is `R_AnimateLight`'s letter `i mod len`.
/// [`LerpLightStyles::Smooth`] moves from that letter toward the next one
/// (`(i + 1) mod len`, the pattern wrapping round) by `frac`, rounded to a
/// whole [`GLIDE_STEP`]: letters are multiples of 22 units, so the value is
/// always a whole unit, never passes the next letter, and is the letter
/// exactly at `frac == 0`, where both are id's. A one-letter pattern (a
/// steady or switched light) never changes, and a pattern QuakeC replaces
/// (`lightstyle()`, a light switched on or off) jumps to the new one at
/// once, in both: there is nothing between two strings to glide along.
fn lightstyle_value(map: &[u8], i: i64, frac: f32, lerp: LerpLightStyles) -> i32 {
    if map.is_empty() {
        return 256; // unset: R_AnimateLight's `length == 0` default
    }
    let len = map.len();
    // A positive modulo keeps k in 0..len for a negative phase (a time
    // before 0).
    let k = i.rem_euclid(len as i64) as usize;
    let here = lightstyle_letter_value(map[k]);
    match lerp {
        LerpLightStyles::Classic => here,
        LerpLightStyles::Smooth => {
            let next = lightstyle_letter_value(map[(k + 1) % len]);
            let steps = ((next - here) as f32 * frac / GLIDE_STEP as f32).round() as i32;
            here + steps * GLIDE_STEP
        }
    }
}

/// `map`'s value at `letters` along it — the letter `floor(letters)`, gliding
/// toward the next by the fraction ([`lightstyle_value`]): a light style is
/// at `time * 10`. The steady torches' flicker (`render::torch`) reads
/// world.qc's flicker patterns through it, each at its own phase.
pub(crate) fn lightstyle_value_at(map: &[u8], letters: f64, lerp: LerpLightStyles) -> i32 {
    let p = if letters.is_finite() { letters } else { 0.0 };
    let i = p.floor();
    lightstyle_value(map, i as i64, (p - i) as f32, lerp)
}

/// `R_AnimateLight` (r_light.c) over an arbitrary style table: the per-style
/// brightness scale at game `time`, one entry per [`MAX_LIGHTSTYLES`] index,
/// stepped as id's or gliding ([`LerpLightStyles`]). Shared by
/// [`Server::lightstyle_scales`] (the live walk's `sv.lightstyles`) and demo
/// playback (the RECORDED `svc_lightstyle` table a `.dem` carries), so both
/// paths animate through the identical logic.
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
///   (so `'a'` → 0 = dark, `'m'` → 264 = normal, `'z'` → 550 ≈ double-bright);
///   gliding, the value between `string[k]` and `string[(k+1) mod L]` by the
///   fraction of `time*10` ([`lightstyle_value`]).
pub fn lightstyle_scales_at(styles: &[String], time: f32, lerp: LerpLightStyles) -> [f32; MAX_LIGHTSTYLES] {
    // Normalise by 256 — id's white point — NOT by 'm' (264). R_AnimateLight
    // sets d_lightstylevalue[j] = (letter-'a')*22 (so worldspawn's lightstyle
    // (0,"m") gives style 0 = 264), and R_BuildLightMap renders luxel*scale
    // against the constant 255*256 white point. So a steady 'm' world is
    // luxel*264/256 = 1.03125x — slightly brighter than a literal luxel*256.
    // Normalising by 'm' (264) made style 0 exactly 1.0, rendering the entire
    // static-lit world ~1 colormap row too dark; /256 matches id. An UNSET style
    // still maps to 1.0 (R_AnimateLight's length==0 default of 256). Every
    // value is a whole number of units, so the scale is exact in f32 and the
    // renderer's `luxel * scale` is id's integer product.
    const NORMAL: f32 = 256.0;
    // The animation phase in characters, floor(time*10) and its fraction. A
    // non-finite time counts as 0; a phase past i64 saturates (the `as` cast)
    // and past f32's range has no fraction, so nothing overflows or panics.
    let p = if time.is_finite() { time * 10.0 } else { 0.0 };
    let i = p.floor() as i64;
    let frac = if p.is_finite() { p - p.floor() } else { 0.0 };
    std::array::from_fn(|j| {
        let map = styles.get(j).map(|s| s.as_bytes()).unwrap_or(b"");
        lightstyle_value(map, i, frac, lerp) as f32 / NORMAL
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
    /// [`crate::render::Scene::light_styles`]; `lerp` says whether a style
    /// steps as id's or glides ([`LerpLightStyles`]).
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
    pub fn lightstyle_scales(&self, time: f32, lerp: LerpLightStyles) -> [f32; MAX_LIGHTSTYLES] {
        // Delegates to the shared table-driven helper so demo playback (the
        // recorded svc_lightstyle table) animates through the IDENTICAL logic.
        lightstyle_scales_at(&self.lightstyles, time, lerp)
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

        let sc = server.lightstyle_scales(0.0, LerpLightStyles::Classic);
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

        let s_t0 = server.lightstyle_scales(0.00, LerpLightStyles::Classic); // k = floor(0)=0 -> 'a' -> 0.0
        let s_t1 = server.lightstyle_scales(0.10, LerpLightStyles::Classic); // k = floor(1)=1 -> 'z' -> ~2.08
        let s_t2 = server.lightstyle_scales(0.20, LerpLightStyles::Classic); // k = floor(2)=0 (mod 2) -> 'a'
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
        let sc = server.lightstyle_scales(0.0, LerpLightStyles::Classic);
        assert!(sc.iter().all(|&s| (s - 1.0).abs() < 1e-6), "no style stored");
        // The valid last in-range index (63) still works as a sanity anchor.
        call_lightstyle(&mut server, 63.0, "a");
        server.run_frame(0.1).expect("frame");
        assert!((server.lightstyle_scales(0.0, LerpLightStyles::Classic)[63] - 0.0).abs() < 1e-6, "index 63 valid");
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
        assert!((s2.lightstyle_scales(0.0, LerpLightStyles::Classic)[1] - 1.0).abs() < 1e-6, "new server style 1 normal");
    }

    // ------------------------------------------- r_lerplightstyles (the 2026 extra)

    /// world.qc's worldspawn: id's twelve animated patterns, styles 0..=11
    /// (style 63, "a", is its test slot).
    const WORLDSPAWN: [&str; 12] = [
        "m",                                                   // 0 normal
        "mmnmmommommnonmmonqnmmo",                             // 1 FLICKER (first variety)
        "abcdefghijklmnopqrstuvwxyzyxwvutsrqponmlkjihgfedcba", // 2 SLOW STRONG PULSE
        "mmmmmaaaaammmmmaaaaaabcdefgabcdefg",                  // 3 CANDLE (first variety)
        "mamamamamama",                                        // 4 FAST STROBE
        "jklmnopqrstuvwxyzyxwvutsrqponmlkj",                   // 5 GENTLE PULSE 1
        "nmonqnmomnmomomno",                                   // 6 FLICKER (second variety)
        "mmmaaaabcdefgmmmmaaaammmaamm",                        // 7 CANDLE (second variety)
        "mmmaaammmaaammmabcdefaaaammmmabcdefmmmaaaa",          // 8 CANDLE (third variety)
        "aaaaaaaazzzzzzzz",                                    // 9 SLOW STROBE (fourth variety)
        "mmamammmmammamamaaamammma",                           // 10 FLUORESCENT FLICKER
        "abcdefghijklmnopqrrqponmlkjihgfedcba",                // 11 SLOW PULSE NOT FADE TO BLACK
    ];

    fn worldspawn_table() -> Vec<String> {
        let mut t: Vec<String> = WORLDSPAWN.iter().map(|s| s.to_string()).collect();
        t.resize(MAX_LIGHTSTYLES, String::new());
        t[63] = "a".into();
        t
    }

    /// Letter `n` of `map` (wrapping): `R_AnimateLight`'s value for the tenth
    /// `n`, `(map[n % len] - 'a') * 22`.
    fn letter(map: &str, n: i64) -> i32 {
        i32::from(map.as_bytes()[n.rem_euclid(map.len() as i64) as usize] - b'a') * 22
    }

    /// A scale back in id's integer units (exact: every scale is n/256).
    fn units(scale: f32) -> i32 {
        let u = scale * 256.0;
        assert_eq!(u, u.round(), "{scale} is a whole number of light units");
        u as i32
    }

    use LerpLightStyles::{Classic, Smooth};

    /// The worldspawn patterns are the shareware progs' own (when id's pak is
    /// here): what `lightstyle()` stores on e1m1 is this table.
    #[test]
    fn the_worldspawn_patterns_are_ids_progs() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../quake-data/ID1/PAK0.PAK");
        let Ok(pak) = crate::pak::Pak::open(&path) else {
            eprintln!("skipped: no shareware pak at {}", path.display());
            return;
        };
        let read = |n: &str| pak.read_file(n).expect("read").expect(n);
        let bsp = crate::bsp::Bsp::parse(&read("maps/e1m1.bsp")).expect("e1m1");
        let progs = Progs::parse(&read("progs.dat")).expect("progs");
        let mut server = Server::with_pak(bsp, progs, Some(pak.clone())).expect("server");
        server.set_map_name("e1m1");
        server.spawn_entities().expect("spawn");
        for (j, want) in worldspawn_table().iter().enumerate() {
            // e1m1's switchable lights (32..) are the map's own, not worldspawn's.
            if j < 12 || j == 63 {
                assert_eq!(server.lightstyle(j), want, "style {j}");
            }
        }
    }

    /// Classic is `R_AnimateLight` as the port always ran it: the letter of
    /// tenth `floor(time*10)` on the f32 clock, before 0 too.
    #[test]
    fn classic_is_r_animatelight() {
        let table = worldspawn_table();
        for n in -500..5000 {
            let t = n as f32 * 0.0137;
            let sc = lightstyle_scales_at(&table, t, Classic);
            let tenth = (t * 10.0).floor() as i64;
            for (j, map) in WORLDSPAWN.iter().enumerate() {
                assert_eq!(units(sc[j]), letter(map, tenth), "style {j} at t = {t}");
            }
            assert_eq!(sc[63], 0.0);
            assert!(sc[12..63].iter().all(|&s| s == 1.0), "unset styles are 1.0");
        }
    }

    /// At every whole tenth of a second the glide is id's letter for that
    /// tenth: exactly, for each of the twelve patterns, over 2000 s of
    /// clock. (Where the f32 clock's own `time*10` lands on the whole
    /// number, Classic is that letter too; just below it, Classic still
    /// shows the tenth before, and the glide, at the end of its step, is
    /// already the letter.)
    #[test]
    fn smooth_is_ids_letter_at_every_whole_tenth() {
        let table = worldspawn_table();
        let mut on_the_tenth = 0;
        for n in 0..20_000i64 {
            let t = n as f32 / 10.0;
            let smooth = lightstyle_scales_at(&table, t, Smooth);
            for (j, map) in WORLDSPAWN.iter().enumerate() {
                assert_eq!(units(smooth[j]), letter(map, n), "style {j} at tenth {n}");
            }
            if (t * 10.0).floor() as i64 == n {
                on_the_tenth += 1;
                assert_eq!(smooth, lightstyle_scales_at(&table, t, Classic), "tenth {n}: id's frame");
            }
        }
        assert!(on_the_tenth > 15_000, "most whole tenths are whole on the f32 clock ({on_the_tenth})");
    }

    /// All through a tenth the value is the linear blend of the tenth's two
    /// letters to within half a step, in whole steps from the first, never
    /// outside the two — halfway, the midpoint to within a unit (a multiple
    /// of 11, the steps even) — and a one-letter step (22 units) takes 11
    /// steps.
    #[test]
    fn smooth_blends_to_the_next_letter_in_steps() {
        let table = worldspawn_table();
        for n in 0..1000i64 {
            let mid = lightstyle_scales_at(&table, (n as f32 + 0.5) / 10.0, Smooth);
            for (j, map) in WORLDSPAWN.iter().enumerate() {
                let want = (letter(map, n) + letter(map, n + 1)) / 2;
                assert!((units(mid[j]) - want).abs() <= GLIDE_STEP / 2, "style {j}, tenth {n}.5: {} vs {want}", units(mid[j]));
            }
        }
        // 4800 Hz over 6 s.
        for f in 0..28_800 {
            let t = f as f32 / 4800.0;
            let p = t * 10.0;
            let (n, frac) = (p.floor() as i64, p - p.floor());
            let sc = lightstyle_scales_at(&table, t, Smooth);
            for (j, map) in WORLDSPAWN.iter().enumerate() {
                let (a, b) = (letter(map, n), letter(map, n + 1));
                let v = units(sc[j]);
                assert!(v >= a.min(b) && v <= a.max(b), "style {j} at {t}: {v} outside {a}..{b}");
                assert_eq!((v - a) % GLIDE_STEP, 0, "style {j} at {t}: whole steps from {a}");
                let exact = a as f32 + (b - a) as f32 * frac;
                assert!((v as f32 - exact).abs() <= GLIDE_STEP as f32 / 2.0 + 1e-3, "style {j} at {t}");
            }
        }
        // The flicker's tenth 1, 'm' (264) to 'n' (286), at 4800 Hz.
        let values: std::collections::BTreeSet<i32> =
            (480..960).map(|f| units(lightstyle_scales_at(&table, f as f32 / 4800.0, Smooth)[1])).collect();
        assert_eq!(values.into_iter().collect::<Vec<_>>(), (264..=286).step_by(2).collect::<Vec<_>>());
    }

    /// A one-letter pattern (a steady light, or a switched one, "a"/"m") never
    /// changes, unset styles stay 1.0, and a pattern QuakeC replaces is the
    /// new one at once: there is nothing between two strings to glide along.
    #[test]
    fn one_letter_patterns_hold_and_a_replaced_pattern_snaps() {
        let table = worldspawn_table();
        for f in 0..2400 {
            let sc = lightstyle_scales_at(&table, f as f32 / 240.0, Smooth);
            assert_eq!(sc[0], 264.0 / 256.0, "'m' is steady");
            assert_eq!(sc[63], 0.0, "'a' is steady");
            assert!(sc[12..63].iter().all(|&s| s == 1.0), "unset styles are 1.0");
        }
        assert_eq!(lightstyle_scales_at(&[], 0.35, Smooth), [1.0; MAX_LIGHTSTYLES], "an empty table");

        let (img, _gc, _gd) = changelevel_progs();
        let mut server = Server::new(empty_bsp(), Progs::parse(&img).expect("parse")).expect("server");
        call_lightstyle(&mut server, 1.0, WORLDSPAWN[1]);
        server.run_frame(0.1).expect("frame");
        // Tenth 17 of the flicker is 'n' (286), tenth 18 'q' (352): a quarter
        // of the way, 16.5 units, is 8 steps of 2.
        let t = 1.725;
        assert_eq!(units(server.lightstyle_scales(t, Smooth)[1]), 302);
        // A switch: QuakeC stores "a" — dark from that frame, in both.
        call_lightstyle(&mut server, 1.0, "a");
        server.run_frame(0.1).expect("frame");
        assert_eq!(server.lightstyle_scales(t, Smooth)[1], 0.0);
        assert_eq!(server.lightstyle_scales(t, Classic)[1], 0.0);
    }

    /// Any clock is safe: before 0 the pattern runs backwards through the
    /// same values; a clock that is not finite is time 0; past f32's range
    /// (`time*10` overflows) the phase saturates with no fraction, as
    /// Classic's does. Nothing panics and every value is a letter's or between.
    #[test]
    fn negative_and_huge_times_are_safe() {
        let table = worldspawn_table();
        let at_zero = lightstyle_scales_at(&table, 0.0, Smooth);
        for t in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(lightstyle_scales_at(&table, t, Smooth), at_zero, "{t}");
            assert_eq!(lightstyle_scales_at(&table, t, Classic), lightstyle_scales_at(&table, 0.0, Classic), "{t}");
        }
        for t in [-1e-9, -0.05, -0.1, -123.456, -1e30, 1e30, f32::MAX, f32::MIN, 16_777_217.0] {
            let sc = lightstyle_scales_at(&table, t, Smooth);
            assert!(sc.iter().all(|&s| (0.0..=550.0 / 256.0).contains(&s)), "{t}: {sc:?}");
            for s in sc {
                units(s);
            }
        }
        // Past 2^23 tenths an f32 clock has no fraction left: the glide is
        // id's letter there.
        for t in [1e30, f32::MAX, f32::MIN] {
            assert_eq!(lightstyle_scales_at(&table, t, Smooth), lightstyle_scales_at(&table, t, Classic), "{t}");
        }
        // Before 0: tenth -1 is the pattern's last letter, gliding to its
        // first — SLOW STROBE's z (550) to a, three quarters of the way:
        // 412.5 units down, 206 steps.
        let strobe = lightstyle_scales_at(&table, -0.025, Smooth)[9];
        assert_eq!(units(strobe), 138, "SLOW STROBE's z -> a wrap");
    }
}
