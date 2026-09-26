//! Console commands — cmd.c: the command buffer's lines, their arguments,
//! and the table of commands a console runs.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/cmd.c` (`Cbuf_Execute`'s line splitting,
//! `Cmd_TokenizeString`, `Cmd_Argc`/`Cmd_Argv`/`Cmd_Args`, `Cmd_AddCommand`'s
//! list with `Cmd_Exists` and `Cmd_CompleteCommand`) and `common.c`
//! (`COM_Parse`).
//!
//! id registers each command with `Cmd_AddCommand (name, function)` into one
//! list that `Cmd_ExecuteString` searches, Tab completion walks and nothing
//! else duplicates. The port's list is a slice of [`Command`]s the platform
//! defines, each a name, a line of help and the function that runs it:
//! dispatch ([`find`]), completion ([`complete`]) and the help text all read
//! the one table. The function type is the platform's: this module only
//! needs the names.

/// `Cbuf_Execute`'s split of the command buffer into command lines: at a
/// newline, or at a `;` outside double quotes. Empty lines are dropped.
pub fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let (mut start, mut quotes) = (0, false);
    for (i, c) in text.char_indices() {
        match c {
            '"' => quotes = !quotes,
            ';' if !quotes => {
                lines.push(&text[start..i]);
                start = i + 1;
            }
            '\n' => {
                lines.push(&text[start..i]);
                start = i + 1;
                quotes = false;
            }
            _ => {}
        }
    }
    lines.push(&text[start..]);
    lines.into_iter().filter(|l| !l.trim().is_empty()).collect()
}

/// `Cmd_StuffCmds_f` (quake.rc's `stuffcmds`): the command line's `+`
/// commands as console lines. The arguments are joined with spaces; each `+`
/// starts a command that runs to the next `+` or `-` (so `quake -basedir .
/// +map e1m1 +skill 2` gives `map e1m1 \n` and `skill 2\n`).
pub fn stuff_cmds(args: &[String]) -> String {
    let text = args.join(" ");
    let mut build = String::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find('+') {
        let command = &rest[start + 1..];
        let end = command.find(['+', '-']).unwrap_or(command.len());
        build.push_str(&command[..end]);
        build.push('\n');
        rest = &command[end..];
    }
    build
}

/// `COM_Parse`'s characters that are a token on their own.
fn single_char_token(c: char) -> bool {
    matches!(c, '{' | '}' | ')' | '(' | '\'' | ':')
}

/// `COM_Parse` (common.c): the next token of `s` and what follows it, or
/// `None` when only whitespace and comments are left. A token is a quoted
/// string (the quotes dropped, ending at the next quote or the end), one of
/// `{ } ( ) ' :`, or a run of other characters up to whitespace or one of
/// those; `//` starts a comment to the end of the line.
fn com_parse(mut s: &str) -> Option<(String, &str)> {
    loop {
        s = s.trim_start_matches(|c: char| c <= ' ');
        match s.strip_prefix("//") {
            Some(rest) => s = rest.find('\n').map_or("", |n| &rest[n..]),
            None => break,
        }
    }
    let c = s.chars().next()?;
    if c == '"' {
        let body = &s[1..];
        let end = body.find('"').unwrap_or(body.len());
        let rest = body.get(end + 1..).unwrap_or("");
        return Some((body[..end].to_string(), rest));
    }
    if single_char_token(c) {
        return Some((c.to_string(), &s[c.len_utf8()..]));
    }
    let end = s.find(|c: char| c <= ' ' || single_char_token(c)).unwrap_or(s.len());
    Some((s[..end].to_string(), &s[end..]))
}

/// A command line's arguments: `Cmd_TokenizeString`'s `cmd_argv`, and
/// `cmd_args`, the line after the command's name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Args {
    argv: Vec<String>,
    args: String,
}

impl Args {
    /// `Cmd_TokenizeString`: the tokens of `line` up to its first newline.
    pub fn tokenize(line: &str) -> Args {
        let line = line.split('\n').next().unwrap_or("");
        let mut a = Args::default();
        let mut rest = line;
        while let Some((token, after)) = com_parse(rest) {
            if a.argv.len() == 1 {
                // cmd_args: from the second token (whitespace skipped) on.
                a.args = rest.trim_start_matches(|c: char| c <= ' ').trim_end().to_string();
            }
            a.argv.push(token);
            rest = after;
        }
        a
    }

    /// `Cmd_Argc`.
    pub fn argc(&self) -> usize {
        self.argv.len()
    }

    /// `Cmd_Argv (i)`: an empty string past the end.
    pub fn argv(&self, i: usize) -> &str {
        self.argv.get(i).map_or("", String::as_str)
    }

    /// Every argument, the command's name first.
    pub fn all(&self) -> Vec<&str> {
        self.argv.iter().map(String::as_str).collect()
    }

    /// `Cmd_Args`: everything after the command's name, as typed (quotes
    /// kept).
    pub fn args(&self) -> &str {
        &self.args
    }
}

/// One console command (`Cmd_AddCommand`): its name, a line for the
/// console's list, and what runs it (the platform's function type).
#[derive(Debug, Clone, Copy)]
pub struct Command<F> {
    pub name: &'static str,
    pub help: &'static str,
    pub run: F,
}

/// `Cmd_ExecuteString`'s search: the command called `name` (any case).
pub fn find<'a, F>(table: &'a [Command<F>], name: &str) -> Option<&'a Command<F>> {
    table.iter().find(|c| c.name.eq_ignore_ascii_case(name))
}

/// `Cmd_CompleteCommand`: the first command whose name starts with
/// `partial` (case matters, `Q_strncmp`); nothing for an empty string.
pub fn complete<F>(table: &[Command<F>], partial: &str) -> Option<&'static str> {
    if partial.is_empty() {
        return None;
    }
    table.iter().map(|c| c.name).find(|n| n.starts_with(partial))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_tokenize_string_is_com_parse() {
        let a = Args::tokenize("bind \"w\" \"+forward\"");
        assert_eq!(a.all(), ["bind", "w", "+forward"]);
        assert_eq!(a.args(), "\"w\" \"+forward\"");
        let a = Args::tokenize("  bind   k   impulse 9  // the rest is a comment");
        assert_eq!(a.all(), ["bind", "k", "impulse", "9"]);
        assert_eq!(a.argv(9), "");
        let a = Args::tokenize("echo a:b (c)");
        assert_eq!(a.all(), ["echo", "a", ":", "b", "(", "c", ")"], "COM_Parse's single-character tokens");
        let a = Args::tokenize("name \"Ranger two\"");
        assert_eq!((a.argc(), a.argv(1)), (2, "Ranger two"));
        assert_eq!(Args::tokenize("  ").argc(), 0);
        assert_eq!(Args::tokenize("map e1m1\nmap e1m2").all(), ["map", "e1m1"], "a line ends the command");
        assert_eq!(Args::tokenize("echo \"unterminated").all(), ["echo", "unterminated"]);
    }

    #[test]
    fn stuffcmds_takes_the_plus_commands_of_the_command_line() {
        let args = |s: &str| s.split(' ').map(str::to_string).collect::<Vec<_>>();
        assert_eq!(stuff_cmds(&args("-basedir . +profile classic +map e1m1")), "profile classic \nmap e1m1\n");
        assert_eq!(stuff_cmds(&args("+map e1m1 -window")), "map e1m1 \n", "a - ends it, as id's");
        assert_eq!(stuff_cmds(&[]), "");
    }

    #[test]
    fn cbuf_execute_splits_at_semicolons_outside_quotes_and_newlines() {
        assert_eq!(split_lines("echo a; echo b\necho c"), ["echo a", " echo b", "echo c"]);
        assert_eq!(split_lines("bind x \"echo a; echo b\"; god"), ["bind x \"echo a; echo b\"", " god"]);
        assert_eq!(split_lines("\n;\n"), Vec::<&str>::new());
    }

    #[test]
    fn the_table_finds_and_completes() {
        let table = [
            Command { name: "timedemo", help: "", run: 1 },
            Command { name: "time", help: "", run: 2 },
            Command { name: "god", help: "", run: 3 },
        ];
        assert_eq!(find(&table, "GOD").map(|c| c.run), Some(3));
        assert!(find(&table, "nosuch").is_none());
        assert_eq!(complete(&table, "ti"), Some("timedemo"), "the first in the list, as cmd_functions");
        assert_eq!(complete(&table, ""), None);
    }
}
