//! Entity-lump loading: `ED_LoadFromFile`, and the text parsers it shares with
//! the savegame loader.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources:
//! * `WinQuake/pr_edict.c` — `ED_LoadFromFile` (+ its skill/deathmatch spawn
//!   filter), `ED_ParseEdict`, `ED_ParseEpair`, `ED_NewString`.
//! * `WinQuake/common.c` — `COM_Parse` (the [`Tokenizer`]).
//!
//! The rest of pr_edict.c — the edict/global runtime and the field/function
//! lookups — lives in the crate's `vm.rs` and `progs.rs`; this is the part that
//! needs a [`Server`] (spawn functions run in its VM, then its settle frames).
//! `save.rs` reuses the tokenizer and the epair parsers, as the C's
//! `Host_Loadgame_f` shares `COM_Parse` / `ED_ParseEdict`.

use super::pr_cmds::cvar_value;
use super::{Server, SpawnReport, SETTLE_FRAMETIME};
use crate::math::Vec3;
use crate::progs::EType;
use crate::Result;

// Skill spawnflags (server.h). These mark an entity as absent on a given
// difficulty (or in deathmatch); `ED_LoadFromFile` filters by the current skill.
const SPAWNFLAG_NOT_EASY: i32 = 256;
const SPAWNFLAG_NOT_MEDIUM: i32 = 512;
const SPAWNFLAG_NOT_HARD: i32 = 1024;
const SPAWNFLAG_NOT_DEATHMATCH: i32 = 2048;

// ---------------------------------------------------------------------------
// COM_Parse tokenizer.
// ---------------------------------------------------------------------------

/// A faithful port of `COM_Parse` (common.c) over `&str` bytes: skips
/// whitespace and `//` line comments, returns `"quoted strings"`, the single
/// characters `{ } ( ) ' :`, or a run of non-whitespace as one token.
/// `pub(crate)`: the savegame loader (`save.rs`) parses the `.sav` brace blocks
/// with the same tokenizer, exactly as the C shares `COM_Parse`.
pub(crate) struct Tokenizer<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Tokenizer<'a> {
    pub(crate) fn new(s: &'a str) -> Tokenizer<'a> {
        Tokenizer {
            data: s.as_bytes(),
            pos: 0,
        }
    }

    /// Return the next token, or `None` at end of input. Mirrors `COM_Parse`,
    /// including the `c <= ' '` whitespace test and the special single chars.
    pub(crate) fn next_token(&mut self) -> Option<String> {
        let mut token = Vec::new();

        // skip whitespace (and // comments), looping like the C `goto skipwhite`.
        loop {
            // skip whitespace
            loop {
                let c = *self.data.get(self.pos)?;
                if c > b' ' {
                    break;
                }
                self.pos += 1;
            }
            // skip // comments
            if self.data.get(self.pos) == Some(&b'/')
                && self.data.get(self.pos + 1) == Some(&b'/')
            {
                while let Some(&c) = self.data.get(self.pos) {
                    if c == b'\n' {
                        break;
                    }
                    self.pos += 1;
                }
                continue;
            }
            break;
        }

        let c = *self.data.get(self.pos)?;

        // quoted string
        if c == b'"' {
            self.pos += 1;
            loop {
                match self.data.get(self.pos) {
                    None => break,            // EOF inside a quote: stop cleanly
                    Some(&b'"') => {
                        self.pos += 1; // consume closing quote
                        break;
                    }
                    Some(&ch) => {
                        token.push(ch);
                        self.pos += 1;
                    }
                }
            }
            return Some(String::from_utf8_lossy(&token).into_owned());
        }

        // single-character tokens
        if is_single(c) {
            self.pos += 1;
            return Some(String::from_utf8_lossy(&[c]).into_owned());
        }

        // a regular word: run of chars > 32 that aren't a single-char token.
        while let Some(&ch) = self.data.get(self.pos) {
            token.push(ch);
            self.pos += 1;
            match self.data.get(self.pos) {
                Some(&nc) if is_single(nc) => break,
                Some(&nc) if nc > 32 => continue,
                _ => break,
            }
        }
        Some(String::from_utf8_lossy(&token).into_owned())
    }
}

/// The C `COM_Parse` single-character set: `{ } ( ) ' :`.
fn is_single(c: u8) -> bool {
    matches!(c, b'{' | b'}' | b')' | b'(' | b'\'' | b':')
}

/// `ED_NewString` (pr_edict.c): copy the raw value, translating `\n` to a
/// newline and any other `\x` escape to a literal backslash. `pub(crate)` for
/// the savegame loader's `ED_ParseEpair` (the C shares this helper too).
pub(crate) fn ed_new_string(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\\' && i < bytes.len() - 1 {
            i += 1;
            if bytes[i] == b'n' {
                out.push('\n');
            } else {
                out.push('\\');
            }
        } else {
            out.push(c as char);
        }
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// value parsers (atof / atoi semantics).
// ---------------------------------------------------------------------------

/// `atof`-like float parse: take the leading numeric prefix, default 0.0. C's
/// `atof` stops at the first non-numeric char and never errors. `pub(crate)`
/// for the savegame loader's `ED_ParseEpair` / header floats.
pub(crate) fn parse_float(s: &str) -> f32 {
    let t = s.trim_start();
    // Find the longest leading prefix that parses; fall back to 0.0.
    let bytes = t.as_bytes();
    let mut end = 0;
    let mut seen_dot = false;
    let mut seen_e = false;
    while end < bytes.len() {
        let c = bytes[end];
        let ok = match c {
            b'0'..=b'9' => true,
            b'+' | b'-' => end == 0 || bytes[end - 1] == b'e' || bytes[end - 1] == b'E',
            b'.' if !seen_dot && !seen_e => {
                seen_dot = true;
                true
            }
            b'e' | b'E' if !seen_e && end > 0 => {
                seen_e = true;
                true
            }
            _ => false,
        };
        if !ok {
            break;
        }
        end += 1;
    }
    t.get(..end).and_then(|p| p.parse::<f32>().ok()).unwrap_or(0.0)
}

/// `atoi`-like int parse: leading optional sign then digits, default 0.
/// `pub(crate)` for the savegame loader's `ev_entity` epair.
pub(crate) fn parse_int(s: &str) -> i32 {
    let t = s.trim_start();
    let bytes = t.as_bytes();
    let mut end = 0;
    while end < bytes.len() {
        let c = bytes[end];
        let ok = c.is_ascii_digit() || ((c == b'+' || c == b'-') && end == 0);
        if !ok {
            break;
        }
        end += 1;
    }
    t.get(..end).and_then(|p| p.parse::<i32>().ok()).unwrap_or(0)
}

/// Parse a "x y z" vector, `atof`-style on each of the first three
/// space-separated fields (missing fields are 0.0), matching `ED_ParseEpair`'s
/// `ev_vector` loop. `pub(crate)` for the savegame loader's `ev_vector` epair.
pub(crate) fn parse_vector(s: &str) -> Vec3 {
    let mut out = [0.0f32; 3];
    for (i, field) in s.split_whitespace().take(3).enumerate() {
        out[i] = parse_float(field);
    }
    out
}

/// Increment the running count for `classname`.
fn bump_classname(counts: &mut Vec<(String, usize)>, classname: &str) {
    if let Some(entry) = counts.iter_mut().find(|(c, _)| c == classname) {
        entry.1 += 1;
    } else {
        counts.push((classname.to_string(), 1));
    }
}

impl Server {
    /// `ED_LoadFromFile` (pr_edict.c): tokenize `bsp.entities`, spawn each
    /// entity, set its fields by name, and call its spawn function (named by
    /// `classname`). Faithful to the C control flow: a spawn function's program
    /// error ends the load (id's `Host_Error`) and is returned.
    pub fn spawn_entities(&mut self) -> Result<SpawnReport> {
        // SV_SpawnServer: current_skill = (int)(skill.value + 0.5), clamped to
        // 0..3, then Cvar_SetValue("skill", current_skill). Re-normalise the live
        // skill the same way before loading entities so a fractional value a
        // front-end set (or a portal's cvar_set) is rounded to the integer the
        // spawn filter compares against, and cvar("skill") reads back the
        // canonical value.
        let skill = self.skill();
        self.set_skill(skill as f32);

        // The entity text was captured at construction (the host has no accessor
        // and we never downcast). Clone it so the tokenizer borrow does not pin
        // `&self`, leaving the VM free to mutate during spawning.
        let entities = self.entities.clone();

        let mut report = SpawnReport::default();
        let mut classname_counts: Vec<(String, usize)> = Vec::new();
        let time = self.time();

        let mut tok = Tokenizer::new(&entities);
        let mut first = true;

        // Loop over entity blocks until EOF (next_token() returns None).
        while let Some(open) = tok.next_token() {
            if open != "{" {
                // C: Sys_Error("found %s when expecting {"). Stay total: stop.
                break;
            }

            report.total += 1;

            // First entity is the world (edict 0); subsequent are ED_Alloc'd.
            let ent = if first {
                first = false;
                0
            } else {
                self.vm.spawn()
            };

            // Parse the key/value pairs into this edict.
            self.parse_edict(&mut tok, ent)?;

            // Skill / deathmatch filtering (ED_LoadFromFile, pr_edict.c). In
            // deathmatch, drop NOT_DEATHMATCH entities; otherwise drop the entity
            // whose NOT_<difficulty> flag matches the current skill (easy=0,
            // medium=1, hard/nightmare>=2). `current_skill` is the live `skill`
            // cvar ([`super::ServerCvars`]) — a difficulty portal's
            // `cvar_set("skill", N)` changes which monsters/items this filter keeps.
            let spawnflags = self.vm.ent_get_float(ent, "spawnflags") as i32;
            let deathmatch = cvar_value(&self.cvars(), "deathmatch") != 0.0;
            let current_skill = self.skill();
            let inhibited = if deathmatch {
                spawnflags & SPAWNFLAG_NOT_DEATHMATCH != 0
            } else {
                (current_skill == 0 && spawnflags & SPAWNFLAG_NOT_EASY != 0)
                    || (current_skill == 1 && spawnflags & SPAWNFLAG_NOT_MEDIUM != 0)
                    || (current_skill >= 2 && spawnflags & SPAWNFLAG_NOT_HARD != 0)
            };
            if inhibited {
                self.vm.free_edict(ent);
                report.inhibited += 1;
                continue;
            }

            // classname -> spawn function.
            let classname = self.vm.ent_get_string(ent, "classname");
            if classname.is_empty() {
                // C: "No classname" -> free and continue.
                self.vm.free_edict(ent);
                continue;
            }
            bump_classname(&mut classname_counts, &classname);

            let func = self.vm.progs().find_function(&classname);
            let Some(func) = func else {
                report.no_spawn_function += 1;
                self.vm.free_edict(ent);
                continue;
            };

            // self = ent, other = world, time = current; then execute. The host
            // is PRESENT here (we are not inside with_host).
            self.vm.gset_int("self", ent);
            self.vm.gset_int("other", 0);
            self.vm.gset_float("time", time);

            // A spawn function's error is Host_Error: the load ends there.
            self.vm.execute(func)?;
            report.spawned += 1;
        }

        // Worldspawn (and any other spawn function) may have called lightstyle();
        // apply those patterns to the owned table.
        self.apply_lightstyles();

        // SV_SpawnServer: "run two frames to allow everything to settle" with
        // host_frametime = 0.1. The first frame fires each entity's spawn-set
        // `nextthink` (e.g. monsters droptofloor / set their first animation
        // frame, items settle onto the floor) so the world is in its resting
        // initial state before play begins. A program error there ends the load,
        // as it ended id's.
        self.run_frame_f64(SETTLE_FRAMETIME)?;
        self.run_frame_f64(SETTLE_FRAMETIME)?;

        // classnames sorted by count desc, then name asc for determinism.
        classname_counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        report.classnames = classname_counts;
        Ok(report)
    }

    /// `ED_ParseEdict` (pr_edict.c): read key/value pairs until `}`, applying the
    /// `angle`/`light`/leading-`_` key hacks, and set each field by its def type.
    fn parse_edict(&mut self, tok: &mut Tokenizer, ent: i32) -> Result<()> {
        // Parse key (or closing brace); EOF here is C's Sys_Error, we stay total.
        while let Some(key) = tok.next_token() {
            if key == "}" {
                break;
            }

            // anglehack: "angle" -> key "angles", value rewritten "0 <v> 0".
            let anglehack = key == "angle";
            // "light" -> "light_lev"; trailing-space trim on the key name.
            let mut keyname = if key == "angle" {
                "angles".to_string()
            } else if key == "light" {
                "light_lev".to_string()
            } else {
                key
            };
            while keyname.ends_with(' ') {
                keyname.pop();
            }

            // parse value
            let value = match tok.next_token() {
                Some(t) => t,
                None => break,
            };
            if value == "}" {
                // C: Sys_Error("closing brace without data"). Stay total.
                break;
            }

            // leading underscore keys are utility comments, discarded.
            if keyname.starts_with('_') {
                continue;
            }

            let value = if anglehack {
                format!("0 {value} 0")
            } else {
                value
            };

            // Set the field by name using its def TYPE; unknown keys are skipped
            // (the C printed "'%s' is not a field" and continued).
            self.set_field(ent, &keyname, &value);
        }
        Ok(())
    }

    /// `ED_ParseEpair` (pr_edict.c): write `value` into entity `ent`'s field
    /// `keyname` according to the field def's type. Unknown fields are silently
    /// skipped (the C continued past them).
    fn set_field(&mut self, ent: i32, keyname: &str, value: &str) {
        // Copy the def's ofs/type out so the immutable `progs` borrow ends before
        // we mutate the VM (intern / set_e*). A missing field is skipped.
        let (ofs, etype) = match self.vm.progs().find_field(keyname) {
            Some(def) => (def.ofs as usize, def.etype()),
            None => return, // not a field — skip (C: "is not a field")
        };
        match etype {
            EType::String => {
                let interned = ed_new_string(value);
                let s_t = self.vm.intern(&interned);
                self.vm.set_ei(ent, ofs, s_t);
            }
            EType::Float => {
                let f = parse_float(value);
                self.vm.set_ef(ent, ofs, f);
            }
            EType::Vector => {
                let v = parse_vector(value);
                self.vm.set_ev(ent, ofs, v);
            }
            EType::Entity => {
                let n = parse_int(value);
                self.vm.set_ei(ent, ofs, n);
            }
            EType::Field => {
                // ev_field: store the ofs of the named field (G_INT(def->ofs)).
                let target_ofs = self.vm.progs().find_field(value).map(|d| d.ofs as i32);
                if let Some(target_ofs) = target_ofs {
                    self.vm.set_ei(ent, ofs, target_ofs);
                }
            }
            EType::Function => {
                // ev_function: store the function index found by name.
                let fnum = self.vm.progs().find_function(value);
                if let Some(fnum) = fnum {
                    self.vm.set_ei(ent, ofs, fnum as i32);
                }
            }
            // ev_void / ev_pointer: nothing to store.
            EType::Void | EType::Pointer => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progs::Progs;
    use crate::server::testutil::*;

    // -------------------------------------------------------------- tokenizer

    #[test]
    fn tokenizer_basic_pairs_and_braces() {
        let mut t = Tokenizer::new("{ \"classname\" \"worldspawn\" }");
        assert_eq!(t.next_token().as_deref(), Some("{"));
        assert_eq!(t.next_token().as_deref(), Some("classname"));
        assert_eq!(t.next_token().as_deref(), Some("worldspawn"));
        assert_eq!(t.next_token().as_deref(), Some("}"));
        assert_eq!(t.next_token(), None);
    }

    #[test]
    fn tokenizer_skips_line_comments_and_words() {
        let mut t = Tokenizer::new("// a comment\nword1   word2\n{ }");
        assert_eq!(t.next_token().as_deref(), Some("word1"));
        assert_eq!(t.next_token().as_deref(), Some("word2"));
        assert_eq!(t.next_token().as_deref(), Some("{"));
        assert_eq!(t.next_token().as_deref(), Some("}"));
        assert_eq!(t.next_token(), None);
    }

    #[test]
    fn tokenizer_unterminated_quote_is_total() {
        // A quote with no closing " must end the token at EOF, not loop/panic.
        let mut t = Tokenizer::new("\"unterminated");
        assert_eq!(t.next_token().as_deref(), Some("unterminated"));
        assert_eq!(t.next_token(), None);
    }

    #[test]
    fn ed_new_string_handles_escapes() {
        assert_eq!(ed_new_string("a\\nb"), "a\nb");
        assert_eq!(ed_new_string("a\\tb"), "a\\b"); // non-n escape -> backslash
        assert_eq!(ed_new_string("plain"), "plain");
    }

    #[test]
    fn parse_helpers() {
        assert_eq!(parse_float("3.5 abc"), 3.5);
        assert_eq!(parse_float("notanumber"), 0.0);
        assert_eq!(parse_int("-42 x"), -42);
        assert_eq!(parse_vector("1 2 3"), [1.0, 2.0, 3.0]);
        assert_eq!(parse_vector("1 2"), [1.0, 2.0, 0.0]);
    }

    // ------------------------------------------------------------ spawn flow

    #[test]
    fn spawn_entities_runs_spawn_function() {
        let (img, _marker, g_one) = marker_progs();
        let progs = Progs::parse(&img).expect("parse progs");
        let ents = "{ \"classname\" \"worldspawn\" }\n{ \"classname\" \"marker\" }\n";
        let bsp = bsp_with_entities(ents);

        let mut server = Server::new(bsp, progs).expect("server");
        // Set the constant 1.0 the marker spawn stores into spawned_flag.
        server.vm.set_gf(g_one, 1.0);

        let report = server.spawn_entities().expect("spawn");

        // Two entity blocks; "marker" has a spawn function, "worldspawn" does
        // not (no function named worldspawn) -> no_spawn_function = 1, but it is
        // edict 0 (world) and freeing it is a no-op.
        assert_eq!(report.total, 2);
        assert_eq!(report.spawned, 1, "marker spawn function ran");
        assert_eq!(report.no_spawn_function, 1, "worldspawn has no spawn fn");

        // The spawn function set the global flag to 1.0.
        assert_eq!(
            server.vm.gget_float("spawned_flag"),
            1.0,
            "marker's spawn function executed and set the global"
        );

        // classnames report contains both, sorted by count desc then name.
        assert!(report
            .classnames
            .iter()
            .any(|(c, n)| c == "marker" && *n == 1));
    }

    #[test]
    fn spawn_entities_inhibits_not_medium() {
        let (img, _marker, g_one) = marker_progs();
        let progs = Progs::parse(&img).expect("parse progs");
        // Entity with spawnflags NOT_MEDIUM (512) must be inhibited at skill 1.
        let ents = "{ \"classname\" \"marker\" \"spawnflags\" \"512\" }\n";
        let bsp = bsp_with_entities(ents);

        let mut server = Server::new(bsp, progs).expect("server");
        server.vm.set_gf(g_one, 1.0);

        let report = server.spawn_entities().expect("spawn");
        assert_eq!(report.total, 1);
        assert_eq!(report.inhibited, 1, "NOT_MEDIUM entity inhibited at skill 1");
        assert_eq!(report.spawned, 0);
        // The spawn function must NOT have run.
        assert_eq!(server.vm.gget_float("spawned_flag"), 0.0);
    }

    #[test]
    fn spawn_entities_bad_entity_does_not_abort() {
        // An entity whose classname has no spawn function is counted, not fatal,
        // and the following good entity still spawns.
        let (img, _marker, g_one) = marker_progs();
        let progs = Progs::parse(&img).expect("parse progs");
        let ents = "{ \"classname\" \"unknown_thing\" }\n{ \"classname\" \"marker\" }\n";
        let bsp = bsp_with_entities(ents);

        let mut server = Server::new(bsp, progs).expect("server");
        server.vm.set_gf(g_one, 1.0);

        let report = server.spawn_entities().expect("spawn");
        assert_eq!(report.total, 2);
        assert_eq!(report.no_spawn_function, 1);
        assert_eq!(report.spawned, 1);
    }

    #[test]
    fn skill_filter_honours_each_difficulty_flag() {
        // FIX-1: the spawn filter must honour NOT_EASY (skill 0), NOT_MEDIUM
        // (skill 1) and NOT_HARD (skill>=2), not just NOT_MEDIUM. An entity
        // flagged NOT_HARD must spawn at skill 0/1 and be inhibited at skill 2/3.
        let make = |skill: f32, spawnflags: i32| -> SpawnReport {
            let (img, _marker, g_one) = marker_progs();
            let progs = Progs::parse(&img).expect("parse");
            let ents = format!(
                "{{ \"classname\" \"marker\" \"spawnflags\" \"{spawnflags}\" }}\n"
            );
            let mut server = Server::new(bsp_with_entities(&ents), progs).expect("server");
            server.vm.set_gf(g_one, 1.0);
            server.set_skill(skill);
            server.spawn_entities().expect("spawn")
        };

        // NOT_HARD (1024): kept on easy/medium, dropped on hard/nightmare.
        assert_eq!(make(0.0, 1024).inhibited, 0, "NOT_HARD spawns at easy");
        assert_eq!(make(1.0, 1024).inhibited, 0, "NOT_HARD spawns at medium");
        assert_eq!(make(2.0, 1024).inhibited, 1, "NOT_HARD inhibited at hard");
        assert_eq!(make(3.0, 1024).inhibited, 1, "NOT_HARD inhibited at nightmare");

        // NOT_EASY (256): dropped only on easy.
        assert_eq!(make(0.0, 256).inhibited, 1, "NOT_EASY inhibited at easy");
        assert_eq!(make(1.0, 256).inhibited, 0, "NOT_EASY spawns at medium");
        assert_eq!(make(2.0, 256).inhibited, 0, "NOT_EASY spawns at hard");

        // NOT_MEDIUM (512): dropped only on medium (the prior behaviour, intact).
        assert_eq!(make(0.0, 512).inhibited, 0, "NOT_MEDIUM spawns at easy");
        assert_eq!(make(1.0, 512).inhibited, 1, "NOT_MEDIUM inhibited at medium");
        assert_eq!(make(2.0, 512).inhibited, 0, "NOT_MEDIUM spawns at hard");

        // A monster with no skill flags always spawns.
        assert_eq!(make(2.0, 0).spawned, 1, "unflagged entity spawns at any skill");
    }
}
