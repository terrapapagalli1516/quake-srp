//! The QuakeC bytecode program format (`progs.dat`).
//!
//! Ported from `pr_comp.h`, `progs.h`, and the `PR_LoadProgs` loader in
//! `pr_edict.c`. A `progs.dat` is a little-endian image with a 60-byte header
//! followed by six sections: statements, global defs, field defs, functions, a
//! string heap, and the initial global values.
//!
//! This module is pure data + decoding; execution lives in [`crate::vm`].

use crate::error::{QError, Result};
use crate::read::Reader;

/// `PROG_VERSION` — the only bytecode version the original engine accepts.
pub const PROG_VERSION: i32 = 6;

/// Reserved global slots, matching `pr_comp.h`.
pub const OFS_NULL: usize = 0;
pub const OFS_RETURN: usize = 1;
pub const OFS_PARM0: usize = 4; // each parameter reserves 3 slots (room for a vector)
pub const OFS_PARM1: usize = 7;
pub const OFS_PARM2: usize = 10;
pub const OFS_PARM3: usize = 13;
pub const OFS_PARM4: usize = 16;
pub const OFS_PARM5: usize = 19;
pub const OFS_PARM6: usize = 22;
pub const OFS_PARM7: usize = 25;
pub const RESERVED_OFS: usize = 28;

/// Maximum number of parameters a function can take.
pub const MAX_PARMS: usize = 8;

/// `ddef_t.type` bit marking a global to be written to savegames.
pub const DEF_SAVEGLOBAL: u16 = 1 << 15;

const STATEMENT_SIZE: usize = 8; // u16 op + 3 * i16
const DEF_SIZE: usize = 8; // u16 type + u16 ofs + i32 s_name
const FUNCTION_SIZE: usize = 36; // 7 * i32 + 8 * u8
const HEADER_SIZE: usize = 60; // 15 * i32

/// QuakeC value type (`etype_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EType {
    Void,
    String,
    Float,
    Vector,
    Entity,
    Field,
    Function,
    Pointer,
}

impl EType {
    pub fn from_u16(v: u16) -> EType {
        match v & !DEF_SAVEGLOBAL {
            0 => EType::Void,
            1 => EType::String,
            2 => EType::Float,
            3 => EType::Vector,
            4 => EType::Entity,
            5 => EType::Field,
            6 => EType::Function,
            _ => EType::Pointer,
        }
    }
    /// Number of 32-bit cells a value of this type occupies (`type_size[]`).
    pub fn cells(self) -> usize {
        match self {
            EType::Vector => 3,
            _ => 1,
        }
    }
}

/// A bytecode instruction (`enum` in `pr_comp.h`), in exact ordinal order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Op {
    Done = 0,
    MulF,
    MulV,
    MulFV,
    MulVF,
    DivF,
    AddF,
    AddV,
    SubF,
    SubV,
    EqF,
    EqV,
    EqS,
    EqE,
    EqFnc,
    NeF,
    NeV,
    NeS,
    NeE,
    NeFnc,
    Le,
    Ge,
    Lt,
    Gt,
    LoadF,
    LoadV,
    LoadS,
    LoadEnt,
    LoadFld,
    LoadFnc,
    Address,
    StoreF,
    StoreV,
    StoreS,
    StoreEnt,
    StoreFld,
    StoreFnc,
    StorepF,
    StorepV,
    StorepS,
    StorepEnt,
    StorepFld,
    StorepFnc,
    Return,
    NotF,
    NotV,
    NotS,
    NotEnt,
    NotFnc,
    If,
    Ifnot,
    Call0,
    Call1,
    Call2,
    Call3,
    Call4,
    Call5,
    Call6,
    Call7,
    Call8,
    State,
    Goto,
    And,
    Or,
    BitAnd,
    BitOr,
}

/// Highest valid opcode ordinal (`OP_BITOR`).
pub const OP_MAX: u16 = Op::BitOr as u16;

impl Op {
    /// Decode an opcode ordinal, or `None` if out of range.
    pub fn from_u16(v: u16) -> Option<Op> {
        if v > OP_MAX {
            return None;
        }
        // Safe: `Op` is `#[repr(u16)]` with contiguous discriminants 0..=OP_MAX,
        // and we just bounds-checked v — but we avoid `unsafe`, so map explicitly.
        Some(OP_TABLE[v as usize])
    }

    /// Mnemonic used by the disassembler.
    pub fn mnemonic(self) -> &'static str {
        OP_NAMES[self as usize]
    }

    /// Number of call arguments for an `OP_CALLn`, else `None`.
    pub fn call_argc(self) -> Option<usize> {
        let v = self as u16;
        if (Op::Call0 as u16..=Op::Call8 as u16).contains(&v) {
            Some((v - Op::Call0 as u16) as usize)
        } else {
            None
        }
    }
}

// A contiguous table so `from_u16` needs no `unsafe` transmute.
const OP_TABLE: [Op; (OP_MAX + 1) as usize] = [
    Op::Done, Op::MulF, Op::MulV, Op::MulFV, Op::MulVF, Op::DivF, Op::AddF, Op::AddV,
    Op::SubF, Op::SubV, Op::EqF, Op::EqV, Op::EqS, Op::EqE, Op::EqFnc, Op::NeF, Op::NeV,
    Op::NeS, Op::NeE, Op::NeFnc, Op::Le, Op::Ge, Op::Lt, Op::Gt, Op::LoadF, Op::LoadV,
    Op::LoadS, Op::LoadEnt, Op::LoadFld, Op::LoadFnc, Op::Address, Op::StoreF, Op::StoreV,
    Op::StoreS, Op::StoreEnt, Op::StoreFld, Op::StoreFnc, Op::StorepF, Op::StorepV,
    Op::StorepS, Op::StorepEnt, Op::StorepFld, Op::StorepFnc, Op::Return, Op::NotF,
    Op::NotV, Op::NotS, Op::NotEnt, Op::NotFnc, Op::If, Op::Ifnot, Op::Call0, Op::Call1,
    Op::Call2, Op::Call3, Op::Call4, Op::Call5, Op::Call6, Op::Call7, Op::Call8,
    Op::State, Op::Goto, Op::And, Op::Or, Op::BitAnd, Op::BitOr,
];

const OP_NAMES: [&str; (OP_MAX + 1) as usize] = [
    "DONE", "MUL_F", "MUL_V", "MUL_FV", "MUL_VF", "DIV_F", "ADD_F", "ADD_V", "SUB_F",
    "SUB_V", "EQ_F", "EQ_V", "EQ_S", "EQ_E", "EQ_FNC", "NE_F", "NE_V", "NE_S", "NE_E",
    "NE_FNC", "LE", "GE", "LT", "GT", "LOAD_F", "LOAD_V", "LOAD_S", "LOAD_ENT",
    "LOAD_FLD", "LOAD_FNC", "ADDRESS", "STORE_F", "STORE_V", "STORE_S", "STORE_ENT",
    "STORE_FLD", "STORE_FNC", "STOREP_F", "STOREP_V", "STOREP_S", "STOREP_ENT",
    "STOREP_FLD", "STOREP_FNC", "RETURN", "NOT_F", "NOT_V", "NOT_S", "NOT_ENT",
    "NOT_FNC", "IF", "IFNOT", "CALL0", "CALL1", "CALL2", "CALL3", "CALL4", "CALL5",
    "CALL6", "CALL7", "CALL8", "STATE", "GOTO", "AND", "OR", "BITAND", "BITOR",
];

/// One bytecode statement (`dstatement_t`, 8 bytes). `a`/`b`/`c` are global slot
/// offsets for most ops, and signed jump offsets for `IF`/`IFNOT`/`GOTO`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Statement {
    pub op: u16,
    pub a: i16,
    pub b: i16,
    pub c: i16,
}

/// A global or field definition (`ddef_t`, 8 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Def {
    pub type_: u16,
    pub ofs: u16,
    pub s_name: i32,
}

impl Def {
    pub fn etype(&self) -> EType {
        EType::from_u16(self.type_)
    }
    pub fn save_global(&self) -> bool {
        self.type_ & DEF_SAVEGLOBAL != 0
    }
}

/// A function record (`dfunction_t`, 36 bytes). A negative `first_statement`
/// means builtin number `-first_statement`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub first_statement: i32,
    pub parm_start: i32,
    pub locals: i32,
    pub profile: i32,
    pub s_name: i32,
    pub s_file: i32,
    pub numparms: i32,
    pub parm_size: [u8; MAX_PARMS],
}

impl Function {
    /// Builtin number if this is a builtin, else `None`.
    pub fn builtin(&self) -> Option<usize> {
        if self.first_statement < 0 {
            Some((-self.first_statement) as usize)
        } else {
            None
        }
    }
}

/// A fully parsed `progs.dat`.
#[derive(Debug, Clone)]
pub struct Progs {
    pub version: i32,
    pub crc: i32,
    pub statements: Vec<Statement>,
    pub globaldefs: Vec<Def>,
    pub fielddefs: Vec<Def>,
    pub functions: Vec<Function>,
    /// The string heap: NUL-separated, indexed by `string_t` byte offset.
    pub strings: Vec<u8>,
    /// Initial values of the global block, one 32-bit cell each.
    pub globals: Vec<u32>,
    /// Number of 32-bit fields per entity.
    pub entityfields: i32,
}

impl Progs {
    /// Parse a `progs.dat` image. Validates the version and that every section
    /// lies within the buffer.
    pub fn parse(bytes: &[u8]) -> Result<Progs> {
        if bytes.len() < HEADER_SIZE {
            return Err(QError::Truncated {
                context: "progs header",
                need: HEADER_SIZE,
                have: bytes.len(),
            });
        }
        let mut h = Reader::new(bytes);
        let version = h.i32()?;
        let crc = h.i32()?;
        let ofs_statements = h.i32()?;
        let numstatements = h.i32()?;
        let ofs_globaldefs = h.i32()?;
        let numglobaldefs = h.i32()?;
        let ofs_fielddefs = h.i32()?;
        let numfielddefs = h.i32()?;
        let ofs_functions = h.i32()?;
        let numfunctions = h.i32()?;
        let ofs_strings = h.i32()?;
        let numstrings = h.i32()?;
        let ofs_globals = h.i32()?;
        let numglobals = h.i32()?;
        let entityfields = h.i32()?;

        if version != PROG_VERSION {
            return Err(QError::invalid(format!(
                "progs.dat wrong version {version} (expected {PROG_VERSION})"
            )));
        }

        let count = |n: i32, what: &'static str| -> Result<usize> {
            if n < 0 {
                Err(QError::invalid(format!("progs.dat negative {what} count: {n}")))
            } else {
                Ok(n as usize)
            }
        };
        let off = |o: i32, what: &'static str| -> Result<usize> {
            if o < 0 {
                Err(QError::invalid(format!("progs.dat negative {what} offset: {o}")))
            } else {
                Ok(o as usize)
            }
        };

        // --- statements ---
        let n = count(numstatements, "statement")?;
        let mut r = Reader::at(bytes, off(ofs_statements, "statements")?);
        let _ = r.slice_at(off(ofs_statements, "statements")?, n * STATEMENT_SIZE)?;
        let mut statements = Vec::with_capacity(n.min(bytes.len() / STATEMENT_SIZE));
        for _ in 0..n {
            statements.push(Statement {
                op: r.u16()?,
                a: r.i16()?,
                b: r.i16()?,
                c: r.i16()?,
            });
        }

        // --- global defs ---
        let n = count(numglobaldefs, "globaldef")?;
        let mut r = Reader::at(bytes, off(ofs_globaldefs, "globaldefs")?);
        let _ = r.slice_at(off(ofs_globaldefs, "globaldefs")?, n * DEF_SIZE)?;
        let mut globaldefs = Vec::with_capacity(n.min(bytes.len() / DEF_SIZE));
        for _ in 0..n {
            globaldefs.push(Def {
                type_: r.u16()?,
                ofs: r.u16()?,
                s_name: r.i32()?,
            });
        }

        // --- field defs ---
        let n = count(numfielddefs, "fielddef")?;
        let mut r = Reader::at(bytes, off(ofs_fielddefs, "fielddefs")?);
        let _ = r.slice_at(off(ofs_fielddefs, "fielddefs")?, n * DEF_SIZE)?;
        let mut fielddefs = Vec::with_capacity(n.min(bytes.len() / DEF_SIZE));
        for _ in 0..n {
            fielddefs.push(Def {
                type_: r.u16()?,
                ofs: r.u16()?,
                s_name: r.i32()?,
            });
        }

        // --- functions ---
        let n = count(numfunctions, "function")?;
        let mut r = Reader::at(bytes, off(ofs_functions, "functions")?);
        let _ = r.slice_at(off(ofs_functions, "functions")?, n * FUNCTION_SIZE)?;
        let mut functions = Vec::with_capacity(n.min(bytes.len() / FUNCTION_SIZE));
        for _ in 0..n {
            functions.push(Function {
                first_statement: r.i32()?,
                parm_start: r.i32()?,
                locals: r.i32()?,
                profile: r.i32()?,
                s_name: r.i32()?,
                s_file: r.i32()?,
                numparms: r.i32()?,
                parm_size: r.bytes::<MAX_PARMS>()?,
            });
        }

        // --- strings (raw NUL-separated heap) ---
        let slen = count(numstrings, "string")?;
        let strings = Reader::new(bytes)
            .slice_at(off(ofs_strings, "strings")?, slen)?
            .to_vec();

        // --- globals (one 32-bit cell each) ---
        let ng = count(numglobals, "global")?;
        let mut r = Reader::at(bytes, off(ofs_globals, "globals")?);
        let _ = r.slice_at(off(ofs_globals, "globals")?, ng * 4)?;
        let mut globals = Vec::with_capacity(ng.min(bytes.len() / 4));
        for _ in 0..ng {
            globals.push(r.u32()?);
        }

        if entityfields < 0 {
            return Err(QError::invalid(format!(
                "progs.dat negative entityfields: {entityfields}"
            )));
        }

        Ok(Progs {
            version,
            crc,
            statements,
            globaldefs,
            fielddefs,
            functions,
            strings,
            globals,
            entityfields,
        })
    }

    /// Resolve a `string_t` (byte offset into the heap) to a `&str`, trimmed at
    /// the first NUL. Out-of-range offsets yield `""`.
    pub fn string(&self, s: i32) -> &str {
        string_in(&self.strings, s)
    }

    /// Index of the function named `name`, if any.
    pub fn find_function(&self, name: &str) -> Option<usize> {
        self.functions
            .iter()
            .position(|f| self.string(f.s_name) == name)
    }

    /// A global def by name.
    pub fn find_global(&self, name: &str) -> Option<&Def> {
        self.globaldefs.iter().find(|d| self.string(d.s_name) == name)
    }

    /// A field def by name.
    pub fn find_field(&self, name: &str) -> Option<&Def> {
        self.fielddefs.iter().find(|d| self.string(d.s_name) == name)
    }

    /// Disassemble one function to text.
    pub fn disassemble_function(&self, idx: usize) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let Some(f) = self.functions.get(idx) else {
            return format!("<no function {idx}>\n");
        };
        let name = self.string(f.s_name);
        let file = self.string(f.s_file);
        if let Some(b) = f.builtin() {
            let _ = writeln!(out, "function {name} = #{b};  // builtin ({file})");
            return out;
        }
        let _ = writeln!(
            out,
            "function {name}()  // {file}, {} parms, {} locals @ {}",
            f.numparms, f.locals, f.parm_start
        );
        let start = f.first_statement.max(0) as usize;
        let mut s = start;
        while s < self.statements.len() {
            let st = self.statements[s];
            let mn = Op::from_u16(st.op)
                .map(|o| o.mnemonic())
                .unwrap_or("<bad op>");
            let _ = writeln!(out, "  {s:5}: {mn:<10} a={} b={} c={}", st.a, st.b, st.c);
            if matches!(Op::from_u16(st.op), Some(Op::Done) | Some(Op::Return)) {
                break;
            }
            s += 1;
        }
        out
    }

    /// Disassemble every non-builtin function.
    pub fn disassemble(&self) -> String {
        let mut out = String::new();
        for i in 1..self.functions.len() {
            out.push_str(&self.disassemble_function(i));
            out.push('\n');
        }
        out
    }
}

/// Read a NUL-terminated string starting at byte offset `s` in `heap`.
pub fn string_in(heap: &[u8], s: i32) -> &str {
    if s < 0 || s as usize >= heap.len() {
        return "";
    }
    let start = s as usize;
    let end = heap[start..]
        .iter()
        .position(|&b| b == 0)
        .map(|p| start + p)
        .unwrap_or(heap.len());
    std::str::from_utf8(&heap[start..end]).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcode_table_is_consistent() {
        // Round-trip every opcode through its ordinal.
        for v in 0..=OP_MAX {
            let op = Op::from_u16(v).expect("valid op");
            assert_eq!(op as u16, v);
        }
        assert!(Op::from_u16(OP_MAX + 1).is_none());
        // Spot-check a few well-known ordinals against pr_comp.h.
        assert_eq!(Op::Done as u16, 0);
        assert_eq!(Op::AddF as u16, 6);
        assert_eq!(Op::Le as u16, 20);
        assert_eq!(Op::Address as u16, 30);
        assert_eq!(Op::Return as u16, 43);
        assert_eq!(Op::If as u16, 49);
        assert_eq!(Op::Call0 as u16, 51);
        assert_eq!(Op::Goto as u16, 61);
        assert_eq!(Op::BitOr as u16, 65);
        assert_eq!(Op::Call3.call_argc(), Some(3));
        assert_eq!(Op::AddF.call_argc(), None);
    }

    /// Build a tiny but structurally valid progs.dat for the loader test.
    fn build_progs() -> Vec<u8> {
        // strings heap
        let mut strings = vec![0u8]; // string 0 = ""
        let name_main = strings.len() as i32;
        strings.extend_from_slice(b"main\0");
        let name_x = strings.len() as i32;
        strings.extend_from_slice(b"x\0");

        // one function "main" starting at statement 0
        let func = Function {
            first_statement: 0,
            parm_start: RESERVED_OFS as i32,
            locals: 0,
            profile: 0,
            s_name: name_main,
            s_file: 0,
            numparms: 0,
            parm_size: [0; 8],
        };
        // function 0 is a null/empty function
        let func0 = Function {
            first_statement: 0,
            parm_start: 0,
            locals: 0,
            profile: 0,
            s_name: 0,
            s_file: 0,
            numparms: 0,
            parm_size: [0; 8],
        };

        let statements = [
            Statement { op: Op::AddF as u16, a: 28, b: 29, c: 30 },
            Statement { op: Op::Done as u16, a: 0, b: 0, c: 0 },
        ];
        // a float global "x" at offset 28 (type 2 == ev_float)
        let globaldefs = [Def { type_: 2, ofs: 28, s_name: name_x }];
        let fielddefs: [Def; 0] = [];
        let functions = [func0, func];
        let globals: Vec<u32> = vec![0u32; 32];

        // layout: header, then sections in order
        fn ser_stmt(s: &Statement) -> Vec<u8> {
            let mut v = Vec::new();
            v.extend_from_slice(&s.op.to_le_bytes());
            v.extend_from_slice(&s.a.to_le_bytes());
            v.extend_from_slice(&s.b.to_le_bytes());
            v.extend_from_slice(&s.c.to_le_bytes());
            v
        }
        fn ser_def(d: &Def) -> Vec<u8> {
            let mut v = Vec::new();
            v.extend_from_slice(&d.type_.to_le_bytes());
            v.extend_from_slice(&d.ofs.to_le_bytes());
            v.extend_from_slice(&d.s_name.to_le_bytes());
            v
        }
        fn ser_func(f: &Function) -> Vec<u8> {
            let mut v = Vec::new();
            for x in [f.first_statement, f.parm_start, f.locals, f.profile, f.s_name, f.s_file, f.numparms] {
                v.extend_from_slice(&x.to_le_bytes());
            }
            v.extend_from_slice(&f.parm_size);
            v
        }

        let mut body = Vec::new();
        let ofs_statements = HEADER_SIZE + body.len();
        for s in &statements { body.extend_from_slice(&ser_stmt(s)); }
        let ofs_globaldefs = HEADER_SIZE + body.len();
        for d in &globaldefs { body.extend_from_slice(&ser_def(d)); }
        let ofs_fielddefs = HEADER_SIZE + body.len();
        for d in &fielddefs { body.extend_from_slice(&ser_def(d)); }
        let ofs_functions = HEADER_SIZE + body.len();
        for f in &functions { body.extend_from_slice(&ser_func(f)); }
        let ofs_strings = HEADER_SIZE + body.len();
        body.extend_from_slice(&strings);
        let ofs_globals = HEADER_SIZE + body.len();
        for g in &globals { body.extend_from_slice(&g.to_le_bytes()); }

        let header: [i32; 15] = [
            PROG_VERSION, 0,
            ofs_statements as i32, statements.len() as i32,
            ofs_globaldefs as i32, globaldefs.len() as i32,
            ofs_fielddefs as i32, fielddefs.len() as i32,
            ofs_functions as i32, functions.len() as i32,
            ofs_strings as i32, strings.len() as i32,
            ofs_globals as i32, globals.len() as i32,
            0, // entityfields
        ];
        let mut out = Vec::new();
        for x in header { out.extend_from_slice(&x.to_le_bytes()); }
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn parses_synthetic_progs() {
        let img = build_progs();
        let p = Progs::parse(&img).expect("parse");
        assert_eq!(p.version, PROG_VERSION);
        assert_eq!(p.statements.len(), 2);
        assert_eq!(p.statements[0].op, Op::AddF as u16);
        assert_eq!(p.functions.len(), 2);
        assert_eq!(p.find_function("main"), Some(1));
        assert_eq!(p.string(p.functions[1].s_name), "main");
        assert!(p.find_global("x").is_some());
        assert_eq!(p.find_global("x").unwrap().ofs, 28);
        // disassembler emits the opcode mnemonic
        let dis = p.disassemble_function(1);
        assert!(dis.contains("ADD_F"), "disasm was: {dis}");
    }

    #[test]
    fn rejects_wrong_version() {
        let mut img = build_progs();
        img[0] = 5; // version
        assert!(Progs::parse(&img).is_err());
    }
}
