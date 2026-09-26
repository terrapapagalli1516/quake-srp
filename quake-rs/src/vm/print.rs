//! What QuakeC's runtime printed to the console when a program went wrong:
//! `PR_RunError`'s report (pr_exec.c: `PR_PrintStatement`, `PR_StackTrace`)
//! and `ED_Print` (pr_edict.c, with `PR_ValueString` and `PR_GlobalString`),
//! which the `error` and `objerror` builtins dump `self` with.
//!
//! Ported to the letter, padding included, so the console reads as id's
//! does. Where id's C would read out of bounds (a bad function or field
//! number in a value) the port prints `???` instead.

use std::fmt::Write as _;

use super::Vm;
use crate::error::ProgramError;
use crate::progs::{string_in, Def, Op, Statement, DEF_SAVEGLOBAL, OP_MAX};

/// `pr_opnames[]` (pr_exec.c): id's opcode names, as `PR_PrintStatement`
/// prints them. The disassembler has its own ([`Op::mnemonic`]): `DIV_F` and
/// `LOAD_F` where id says `DIV` and `INDIRECT`.
const PR_OPNAMES: [&str; OP_MAX as usize + 1] = [
    "DONE", "MUL_F", "MUL_V", "MUL_FV", "MUL_VF", "DIV", "ADD_F", "ADD_V", "SUB_F", "SUB_V",
    "EQ_F", "EQ_V", "EQ_S", "EQ_E", "EQ_FNC", "NE_F", "NE_V", "NE_S", "NE_E", "NE_FNC", "LE",
    "GE", "LT", "GT", "INDIRECT", "INDIRECT", "INDIRECT", "INDIRECT", "INDIRECT", "INDIRECT",
    "ADDRESS", "STORE_F", "STORE_V", "STORE_S", "STORE_ENT", "STORE_FLD", "STORE_FNC",
    "STOREP_F", "STOREP_V", "STOREP_S", "STOREP_ENT", "STOREP_FLD", "STOREP_FNC", "RETURN",
    "NOT_F", "NOT_V", "NOT_S", "NOT_ENT", "NOT_FNC", "IF", "IFNOT", "CALL0", "CALL1", "CALL2",
    "CALL3", "CALL4", "CALL5", "CALL6", "CALL7", "CALL8", "STATE", "GOTO", "AND", "OR", "BITAND",
    "BITOR",
];

/// `type_size[]` (pr_edict.c): how many cells a value of each type fills.
const TYPE_SIZE: [usize; 8] = [1, 1, 1, 3, 1, 1, 1, 1];

/// Pad `line` with spaces to 20 columns, then one more: `PR_GlobalString`'s
/// column.
fn pad20(mut line: String) -> String {
    while line.len() < 20 {
        line.push(' ');
    }
    line.push(' ');
    line
}

impl Vm {
    /// `PR_RunError` (pr_exec.c): the error `message` raised at the current
    /// statement, with the console text id printed before its
    /// `Host_Error ("Program error")` — the statement, the stack trace, the
    /// message.
    pub(super) fn program_error(&self, message: String) -> ProgramError {
        let mut console = String::new();
        if let Some(st) = self.progs.statements.get(self.xstatement) {
            console.push_str(&self.print_statement(st));
        }
        console.push_str(&self.stack_trace());
        console.push_str(&message);
        console.push('\n');
        ProgramError { function: self.running_function().to_string(), message, console }
    }

    /// The name of function `f` (`""` for none or a bad number).
    pub(crate) fn function_name(&self, f: usize) -> &str {
        self.progs.functions.get(f).map_or("", |f| string_in(&self.strings, f.s_name))
    }

    /// The name of the QuakeC function running (`pr_xfunction->s_name`).
    pub(crate) fn running_function(&self) -> &str {
        self.function_name(self.xfunction)
    }

    /// `PR_ValueString` (pr_edict.c): a value of def type `type_` (its
    /// `DEF_SAVEGLOBAL` bit ignored) held in the cells `v`.
    pub(crate) fn value_string(&self, type_: u16, v: [u32; 3]) -> String {
        let f = |c: u32| f32::from_bits(c);
        match type_ & !DEF_SAVEGLOBAL {
            0 => "void".to_string(),
            1 => string_in(&self.strings, v[0] as i32).to_string(),
            2 => format!("{:5.1}", f(v[0])),
            3 => format!("'{:5.1} {:5.1} {:5.1}'", f(v[0]), f(v[1]), f(v[2])),
            4 => format!("entity {}", v[0] as i32),
            5 => {
                let def = self.progs.fielddefs.iter().find(|d| u32::from(d.ofs) == v[0]);
                format!(".{}", def.map_or("???", |d| string_in(&self.strings, d.s_name)))
            }
            6 => match self.progs.functions.get(v[0] as usize) {
                Some(func) => format!("{}()", string_in(&self.strings, func.s_name)),
                None => "???()".to_string(),
            },
            7 => "pointer".to_string(),
            t => format!("bad type {t}"),
        }
    }

    /// `ED_GlobalAtOfs` (pr_edict.c): the first global def at cell `ofs`.
    fn global_at_ofs(&self, ofs: usize) -> Option<&Def> {
        self.progs.globaldefs.iter().find(|d| usize::from(d.ofs) == ofs)
    }

    /// `PR_GlobalString` (pr_edict.c): `ofs(name)value` for global `ofs`,
    /// padded to its column.
    fn global_string(&self, ofs: usize) -> String {
        pad20(match self.global_at_ofs(ofs) {
            None => format!("{ofs}(???)"),
            Some(def) => {
                let cell = |i: usize| self.globals.get(ofs + i).copied().unwrap_or(0);
                let value = self.value_string(def.type_, [cell(0), cell(1), cell(2)]);
                format!("{ofs}({}){value}", string_in(&self.strings, def.s_name))
            }
        })
    }

    /// `PR_GlobalStringNoContents` (pr_edict.c): `ofs(name)`, padded.
    fn global_string_no_contents(&self, ofs: usize) -> String {
        pad20(match self.global_at_ofs(ofs) {
            None => format!("{ofs}(???)"),
            Some(def) => format!("{ofs}({})", string_in(&self.strings, def.s_name)),
        })
    }

    /// `PR_PrintStatement` (pr_exec.c): one statement as `PR_RunError` shows
    /// the one that failed — the opcode, then its operands with their values.
    pub(crate) fn print_statement(&self, st: &Statement) -> String {
        let mut out = String::new();
        if let Some(name) = PR_OPNAMES.get(usize::from(st.op.code())) {
            out.push_str(name);
            out.push(' ');
            for _ in name.len()..10 {
                out.push(' ');
            }
        }
        // The operands are global offsets, except the branch distances.
        let g = |x: i16| usize::from(x as u16);
        if matches!(st.op, Op::If | Op::Ifnot) {
            let _ = write!(out, "{}branch {}", self.global_string(g(st.a)), st.b);
        } else if st.op == Op::Goto {
            let _ = write!(out, "branch {}", st.a);
        } else if matches!(
            // `(unsigned)(s->op - OP_STORE_F) < 6`: the six STORE_*s.
            st.op,
            Op::StoreF | Op::StoreV | Op::StoreS | Op::StoreEnt | Op::StoreFld | Op::StoreFnc
        ) {
            out.push_str(&self.global_string(g(st.a)));
            out.push_str(&self.global_string_no_contents(g(st.b)));
        } else {
            if st.a != 0 {
                out.push_str(&self.global_string(g(st.a)));
            }
            if st.b != 0 {
                out.push_str(&self.global_string(g(st.b)));
            }
            if st.c != 0 {
                out.push_str(&self.global_string_no_contents(g(st.c)));
            }
        }
        out.push('\n');
        out
    }

    /// `PR_StackTrace` (pr_exec.c): the running function, then its callers
    /// out to the entry frame (whose caller is no function: id's
    /// `<NO FUNCTION>`), each as `file : name`.
    pub(crate) fn stack_trace(&self) -> String {
        if self.stack.is_empty() {
            return "<NO STACK>\n".to_string();
        }
        let mut out = String::new();
        let frames = self.stack.iter().map(|fr| fr.f).chain([self.xfunction]);
        for f in frames.collect::<Vec<_>>().into_iter().rev() {
            match self.progs.functions.get(f).filter(|_| f != 0) {
                None => out.push_str("<NO FUNCTION>\n"),
                Some(func) => {
                    let file = string_in(&self.strings, func.s_file);
                    let _ = writeln!(out, "{file:>12} : {}", string_in(&self.strings, func.s_name));
                }
            }
        }
        out
    }

    /// `ED_Print` (pr_edict.c): edict `e` as the console shows it — `FREE`,
    /// or its number and every field that is not all zero (the `_x`/`_y`/`_z`
    /// component names skipped), name padded to 15 columns, then the value.
    pub fn ed_print(&self, e: i32) -> String {
        if self.is_free_edict(e) {
            return "FREE\n".to_string();
        }
        let mut out = format!("\nEDICT {e}:\n");
        for def in self.progs.fielddefs.iter().skip(1) {
            let name = string_in(&self.strings, def.s_name);
            if name.len() >= 2 && name.as_bytes()[name.len() - 2] == b'_' {
                continue; // skip _x, _y, _z vars
            }
            let ofs = usize::from(def.ofs);
            let cells = [self.ei(e, ofs), self.ei(e, ofs + 1), self.ei(e, ofs + 2)].map(|c| c as u32);
            let size = TYPE_SIZE.get(usize::from(def.type_ & !DEF_SAVEGLOBAL)).copied().unwrap_or(1);
            if cells[..size].iter().all(|&c| c == 0) {
                continue; // the value is still all 0
            }
            let _ = writeln!(out, "{name:<15}{}", self.value_string(def.type_, cells));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opnames_are_ids() {
        assert_eq!(PR_OPNAMES[usize::from(Op::DivF.code())], "DIV");
        assert_eq!(PR_OPNAMES[usize::from(Op::LoadV.code())], "INDIRECT");
        assert_eq!(PR_OPNAMES[usize::from(Op::Address.code())], "ADDRESS");
        assert_eq!(PR_OPNAMES[usize::from(Op::BitOr.code())], "BITOR");
    }

    #[test]
    fn pad20_is_ids_column() {
        assert_eq!(pad20("12(self)entity 1".into()), "12(self)entity 1     ");
        assert_eq!(pad20("x".repeat(25)), format!("{} ", "x".repeat(25)));
    }
}
