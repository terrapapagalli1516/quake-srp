//! The 2021 re-release's string localization table, and the substitution
//! `PF_VarString` (`pr_cmds.c`) now does with it.
//!
//! id's shareware/registered `progs.dat` prints every string immediate
//! verbatim: `PF_VarString` is a plain concatenation of its var-args
//! (`builtins.rs`'s original [`crate::builtins::var_string`]). The mission
//! packs' re-release `progs.dat` are recompiles whose `sprint`/`bprint`/
//! `centerprint`/`dprint` string immediates are `$key` localization keys
//! instead of English — `bprint(other, "$qc_got_item", self.netname)` where
//! `self.netname` is itself often a key (`"$qc_double_shotgun"`) — and the
//! re-release's own engine looks each key up in `loc_english.txt`
//! (`key = "text"`, `{0}`/`{1}`/… placeholders) before showing it. id's C
//! and this port's original `PF_VarString` have no such table, so the
//! player reads the raw keys concatenated (AUDIT P6): `"$qc_got_item$qc_
//! double_shotgun"`.
//!
//! This module is that table ([`LocTable`]) and the lookup+substitution
//! [`format`] a var-arg call's own text goes through, plus [`resolve`] for a
//! single already-extracted key (`svc_finale`/`svc_cutscene`'s raw
//! `WriteString` payload, which carries no var-args for `format` to see —
//! see `client/cl_main.rs`, where the finale/cutscene text is finally
//! shown). `loc_english.txt` is the 2021 re-release's own file
//! (`QuakeEX.kpf`); `mission_paks.py` puts a copy into each mission
//! pack's own `pak0.pak` (`localization/loc_english.txt`), so it travels on
//! the same search path as everything else the pack needs — id1's packs
//! carry none, and id1's progs has no `$` string to look up anyway, so a
//! plain shareware/registered game runs through this module with `loc =
//! None` and gets exactly [`crate::builtins::var_string`]'s old behaviour.
//!
//! AUDIT B3 decided this runs in BOTH presets, not just slop: a raw `$key`
//! is a build artefact, never something a player should read, in Classic or
//! not — and since id1's own progs never uses one, Classic's identity checks
//! (`census`, `edicts`, the goldens) do not move either way.

use std::collections::HashMap;

/// A parsed `loc_english.txt`: every `key = "text"` line, keyed WITHOUT the
/// leading `$` a QuakeC string immediate carries (`"$qc_got_item"` looks up
/// `"qc_got_item"`).
#[derive(Debug, Clone, Default)]
pub struct LocTable(HashMap<String, String>);

impl LocTable {
    /// Parse `loc_english.txt`'s text. Each non-blank, non-`//`-comment line
    /// must be `key = "text"` (`key` is `[A-Za-z0-9_]+`) or it is skipped —
    /// the same leniency `mission_paks.py`'s own `load_loc` has, for the
    /// same reason: the file also carries platform-tagged menu keys
    /// (`m_gamepad_restricted <ps4 ps5 switch> = "Controller Only"`) and a
    /// few lines with a trailing `// comment` after the closing quote
    /// (`m_monsters = "Monsters"     // SP, MT`), neither shape this engine
    /// needs (no `qc_*` — the only keys this port's progs ever look up —
    /// line has either). First wording wins on a repeated key (the
    /// re-release folds a couple of 1996 lines into one key more than
    /// once: `qc_antigrav_lost`, `qc_shield_lost`, `qc_vengeance_sphere`,
    /// `qc_your_team_captured`).
    pub fn parse(text: &str) -> LocTable {
        let mut map: HashMap<String, String> = HashMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            let Some(eq) = line.find('=') else { continue };
            let key = line[..eq].trim();
            if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                continue;
            }
            let Some(value) = parse_quoted(line[eq + 1..].trim()) else { continue };
            map.entry(key.to_string()).or_insert_with(|| unescape(&value));
        }
        LocTable(map)
    }

    /// The text for `key` (without its leading `$`), or `None` if the table
    /// has no such entry.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    /// True for a table with no entries (an empty or unreadable
    /// `loc_english.txt`): the caller treats it the same as no table at all.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A `"..."` value up to the LAST `"` on the line, with the remainder
/// (expected to be empty or a stray space) trimmed away — the same
/// assumption `mission_paks.py`'s `r'"(.*)"\s*$'` makes, and why its
/// `m_monsters = "Monsters" // SP, MT` line also fails to match. The file
/// never escapes a `"` inside a value (checked: zero `\"` in
/// `loc_english.txt`), so this never cuts a value short.
fn parse_quoted(s: &str) -> Option<String> {
    let s = s.strip_prefix('"')?;
    let end = s.rfind('"')?;
    s[end + 1..].trim().is_empty().then(|| s[..end].to_string())
}

/// The file's only escape in practice: a literal two-character `\n`
/// becomes a real newline (the loc file is a human-edited text format, not
/// compiled progs bytes — those already carry a true `0x0A`). Any other
/// backslash sequence (none appear in the shipped file) is left exactly as
/// written rather than guessed at.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Load `localization/loc_english.txt` from `pak` — the same search path
/// [`crate::server::Server::with_pak`] was built with — the way
/// `mission_paks.py` puts a copy into each mission pack's own
/// `pak0.pak`. `None` for a pak that carries no such file (id1's: its progs
/// has no `$` string to look up) or whose copy parses to no entry at all.
pub fn load(pak: &crate::pak::Pak) -> Option<LocTable> {
    let bytes = pak.read_file("localization/loc_english.txt").ok().flatten()?;
    let table = LocTable::parse(&String::from_utf8_lossy(&bytes));
    (!table.is_empty()).then_some(table)
}

/// `PF_VarString`'s reading with a loc table (Ironwail's `LOC_Format` is the
/// usual one): `args[0]` is the candidate key. With a table that has it
/// (stripped of its leading `$`), its text's `{0}`, `{1}`, … are replaced by
/// `args[1..]` ([`substitute`]); otherwise — no table, no leading `$`, or a
/// `$key` the table lacks — every argument is concatenated, exactly as the
/// original `PF_VarString`/[`crate::builtins::var_string`] always did (so a
/// plain string, or a key this table doesn't carry, prints the same as
/// before this module existed).
pub fn format(loc: Option<&LocTable>, args: &[String]) -> String {
    if let (Some(loc), Some(key)) = (loc, args.first().and_then(|a| a.strip_prefix('$'))) {
        if let Some(fmt) = loc.get(key) {
            return substitute(fmt, &args[1..], loc);
        }
    }
    args.concat()
}

/// `svc_finale`/`svc_cutscene`'s raw `WriteString` text (and anything else
/// that is a single already-extracted string, never concatenated with
/// var-args): the same `$key` lookup [`format`] gives a call's first
/// argument, just with no following arguments to fill a `{0}` with — which
/// [`substitute`] already renders as a literal placeholder rather than
/// losing it, so a key whose text unexpectedly carries one is still visible,
/// not silently dropped. Used where the text is finally shown, not where
/// the engine wrote it: `WriteString` has no var-arg concatenation step for
/// [`format`] to intercept, unlike `sprint`/`bprint`/`centerprint`.
pub fn resolve(loc: Option<&LocTable>, text: &str) -> String {
    format(loc, &[text.to_string()])
}

/// `fmt`'s `{0}`, `{1}`, … replaced by `args[n]` (each arg looked up in
/// `loc` too if it is itself a `$key` — [`resolve_arg`] — one level only:
/// the substituted text is not re-scanned for more placeholders, since the
/// packs only ever use this for item/noun names). `{{` and `}}` escape a
/// literal brace (never in `loc_english.txt` today, but it is `LOC_Format`'s
/// own reading of the format and free to keep). A `{n}` past the end of
/// `args`, or not a well-formed `{digits}`, is left exactly as written:
/// safer than panicking or silently eating it over a progs/table mismatch.
fn substitute(fmt: &str, args: &[String], loc: &LocTable) -> String {
    let mut out = String::with_capacity(fmt.len());
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let mut digits = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_digit() {
                        digits.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let well_formed = !digits.is_empty() && chars.peek() == Some(&'}');
                if well_formed {
                    chars.next(); // the closing '}'
                }
                match (well_formed, digits.parse::<usize>().ok().and_then(|i| args.get(i))) {
                    (true, Some(arg)) => out.push_str(&resolve_arg(arg, loc)),
                    _ => {
                        out.push('{');
                        out.push_str(&digits);
                        if well_formed {
                            out.push('}');
                        }
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// One substitution argument: a `$key` lookup (one level — `qc_got_item`'s
/// `{0}` filled with `"$qc_double_shotgun"`, itself a key, becomes "the
/// Double-barrelled Shotgun"), or the argument's own text when it isn't a
/// key the table has.
fn resolve_arg(arg: &str, loc: &LocTable) -> String {
    arg.strip_prefix('$').and_then(|k| loc.get(k)).map(str::to_string).unwrap_or_else(|| arg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(pairs: &[(&str, &str)]) -> LocTable {
        let text: String = pairs.iter().map(|(k, v)| format!("{k} = \"{v}\"\n")).collect();
        LocTable::parse(&text)
    }

    #[test]
    fn parses_key_equals_quoted_text() {
        let t = table(&[("qc_double_shotgun", "the Double-barrelled Shotgun")]);
        assert_eq!(t.get("qc_double_shotgun"), Some("the Double-barrelled Shotgun"));
        assert_eq!(t.get("missing"), None);
    }

    #[test]
    fn parse_skips_comments_blanks_and_platform_tagged_lines() {
        let t = LocTable::parse(
            "// a comment\n\n\
             qc_ok = \"fine\"\n\
             m_gamepad_restricted <ps4 ps5 switch> = \"Controller Only\"\n\
             m_monsters = \"Monsters\"     // SP, MT\n",
        );
        assert_eq!(t.get("qc_ok"), Some("fine"));
        assert_eq!(t.get("m_gamepad_restricted"), None);
        assert_eq!(
            t.get("m_monsters"),
            None,
            "a trailing comment breaks the same way mission_paks.py's own parser does"
        );
    }

    #[test]
    fn parse_unescapes_n_and_keeps_first_wording_on_a_duplicate_key() {
        let t = LocTable::parse("qc_x = \"line one\\nline two\"\nqc_x = \"second wording\"\n");
        assert_eq!(t.get("qc_x"), Some("line one\nline two"));
    }

    #[test]
    fn format_with_no_dollar_concatenates_as_before() {
        // dprint/bprint's old behaviour for an ordinary (non-key) string, with
        // or without a table loaded.
        let loc = table(&[("qc_x", "irrelevant")]);
        let args = vec!["plain ".to_string(), "text".to_string()];
        assert_eq!(format(Some(&loc), &args), "plain text");
        assert_eq!(format(None, &args), "plain text");
    }

    #[test]
    fn format_missing_key_falls_back_to_concatenation() {
        // A "$key" the table doesn't have: id's PF_VarString's old behaviour
        // (print the key literally), not a panic or an empty string.
        let loc = table(&[("qc_other", "text")]);
        let args = vec!["$qc_missing".to_string(), "$qc_other".to_string()];
        assert_eq!(format(Some(&loc), &args), "$qc_missing$qc_other");
    }

    #[test]
    fn format_substitutes_positional_and_nested_key_arguments() {
        // bprint(other, "$qc_got_item", "$qc_double_shotgun") -> the AUDIT
        // P6/B3 proof case.
        let loc = table(&[("qc_got_item", "You got {0}\\n"), ("qc_double_shotgun", "the Double-barrelled Shotgun")]);
        let args = vec!["$qc_got_item".to_string(), "$qc_double_shotgun".to_string()];
        assert_eq!(format(Some(&loc), &args), "You got the Double-barrelled Shotgun\n");
    }

    #[test]
    fn format_plain_argument_is_used_verbatim_not_looked_up() {
        let loc = table(&[("qc_got_item", "You got {0}\\n")]);
        let args = vec!["$qc_got_item".to_string(), "the Axe".to_string()];
        assert_eq!(format(Some(&loc), &args), "You got the Axe\n");
    }

    #[test]
    fn format_missing_argument_leaves_the_placeholder_literal() {
        // The table's text asks for {0} but the call supplied no second
        // argument: keep the braces rather than drop them or panic.
        let loc = table(&[("qc_got_item", "You got {0}\\n")]);
        let args = vec!["$qc_got_item".to_string()];
        assert_eq!(format(Some(&loc), &args), "You got {0}\n");
    }

    #[test]
    fn format_escaped_braces_are_literal() {
        let loc = table(&[("qc_braces", "{{literal}} {0}")]);
        let args = vec!["$qc_braces".to_string(), "arg".to_string()];
        assert_eq!(format(Some(&loc), &args), "{literal} arg");
    }

    #[test]
    fn resolve_looks_up_a_single_key_with_no_arguments() {
        // svc_finale/svc_cutscene's raw WriteString payload.
        let loc = table(&[("qc_finale_hip1", "Deep within the bowels\u{2026}")]);
        assert_eq!(resolve(Some(&loc), "$qc_finale_hip1"), "Deep within the bowels\u{2026}");
        assert_eq!(resolve(Some(&loc), "$qc_missing"), "$qc_missing");
        assert_eq!(resolve(None, "$qc_finale_hip1"), "$qc_finale_hip1");
        assert_eq!(resolve(Some(&loc), "plain finale text"), "plain finale text");
    }
}
