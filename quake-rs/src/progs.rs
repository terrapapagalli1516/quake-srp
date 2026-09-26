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

/// In-bounds precheck span: `count * record_size` as a byte length, with the
/// multiply done via [`usize::checked_mul`] so a hostile (untrusted) record
/// count cannot overflow `usize` and wrap to a tiny span that would then pass
/// `slice_at` while the real read runs off the buffer. Any overflow is a clean
/// parse error — never a panic, never a wrapped (wrong) slice. This matters on
/// 32-bit targets (e.g. `wasm32`, where `usize` is 32-bit): there an `i32`
/// count near `i32::MAX` times an 8-byte record already overflows `u32`.
fn table_span(count: usize, record_size: usize, what: &'static str) -> Result<usize> {
    count.checked_mul(record_size).ok_or_else(|| {
        QError::invalid(format!("progs.dat {what} table size overflows"))
    })
}

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

/// Declares [`Op`] from one list of `pr_comp.h`'s opcodes, in their ordinal
/// order, each with its disassembler mnemonic: the enum, the decode table
/// ([`Op::from_code`]), the mnemonics ([`Op::mnemonic`]) and the encode
/// ([`Op::code`]) all come from the same list, so they cannot disagree.
macro_rules! opcodes {
    ($($op:ident $mnemonic:literal),* $(,)?) => {
        /// A bytecode instruction (the opcode `enum` in `pr_comp.h`), decoded
        /// once, when the progs is loaded ([`Progs::parse`]), so the
        /// interpreter matches on it directly.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Op {
            $($op,)*
            /// A number no `pr_comp.h` opcode has. It loads, and faults only
            /// if it runs, as in id's `PR_ExecuteProgram` (its `default:`
            /// arm, `PR_RunError ("Bad opcode %i")`).
            Invalid(u16),
        }

        /// A fieldless twin of [`Op`]'s named opcodes: its discriminants count
        /// from 0 in declaration order, which makes them the `pr_comp.h`
        /// ordinals ([`Op::code`]).
        #[derive(Clone, Copy)]
        #[repr(u16)]
        enum Ordinal {
            $($op,)*
        }

        /// The named opcodes by ordinal: `OP_TABLE[n]` is opcode `n`.
        const OP_TABLE: &[Op] = &[$(Op::$op,)*];

        /// The disassembler's mnemonic of each named opcode, by ordinal.
        const OP_NAMES: &[&str] = &[$($mnemonic,)*];

        impl Op {
            /// The opcode's number in a `dstatement_t` (its `pr_comp.h` ordinal).
            pub const fn code(self) -> u16 {
                match self {
                    $(Op::$op => Ordinal::$op as u16,)*
                    Op::Invalid(code) => code,
                }
            }
        }
    };
}

opcodes!(
    Done "DONE", MulF "MUL_F", MulV "MUL_V", MulFV "MUL_FV", MulVF "MUL_VF", DivF "DIV_F",
    AddF "ADD_F", AddV "ADD_V", SubF "SUB_F", SubV "SUB_V",
    EqF "EQ_F", EqV "EQ_V", EqS "EQ_S", EqE "EQ_E", EqFnc "EQ_FNC",
    NeF "NE_F", NeV "NE_V", NeS "NE_S", NeE "NE_E", NeFnc "NE_FNC",
    Le "LE", Ge "GE", Lt "LT", Gt "GT",
    LoadF "LOAD_F", LoadV "LOAD_V", LoadS "LOAD_S", LoadEnt "LOAD_ENT", LoadFld "LOAD_FLD",
    LoadFnc "LOAD_FNC", Address "ADDRESS",
    StoreF "STORE_F", StoreV "STORE_V", StoreS "STORE_S", StoreEnt "STORE_ENT",
    StoreFld "STORE_FLD", StoreFnc "STORE_FNC",
    StorepF "STOREP_F", StorepV "STOREP_V", StorepS "STOREP_S", StorepEnt "STOREP_ENT",
    StorepFld "STOREP_FLD", StorepFnc "STOREP_FNC",
    Return "RETURN", NotF "NOT_F", NotV "NOT_V", NotS "NOT_S", NotEnt "NOT_ENT", NotFnc "NOT_FNC",
    If "IF", Ifnot "IFNOT",
    Call0 "CALL0", Call1 "CALL1", Call2 "CALL2", Call3 "CALL3", Call4 "CALL4", Call5 "CALL5",
    Call6 "CALL6", Call7 "CALL7", Call8 "CALL8",
    State "STATE", Goto "GOTO", And "AND", Or "OR", BitAnd "BITAND", BitOr "BITOR",
);

/// Highest valid opcode ordinal (`OP_BITOR`).
pub const OP_MAX: u16 = Op::BitOr.code();

impl Op {
    /// Decode an opcode number: the named opcode, or [`Op::Invalid`].
    pub fn from_code(code: u16) -> Op {
        OP_TABLE.get(usize::from(code)).copied().unwrap_or(Op::Invalid(code))
    }

    /// Mnemonic used by the disassembler (`<bad op>` for an invalid one).
    pub fn mnemonic(self) -> &'static str {
        match self {
            Op::Invalid(_) => "<bad op>",
            op => OP_NAMES.get(usize::from(op.code())).copied().unwrap_or("<bad op>"),
        }
    }

    /// Number of call arguments for an `OP_CALLn`, else `None`.
    pub fn call_argc(self) -> Option<usize> {
        match self {
            Op::Call0 | Op::Call1 | Op::Call2 | Op::Call3 | Op::Call4 | Op::Call5 | Op::Call6
            | Op::Call7 | Op::Call8 => Some(usize::from(self.code() - Op::Call0.code())),
            _ => None,
        }
    }
}

/// One bytecode statement (`dstatement_t`, 8 bytes on disk), its opcode
/// decoded at load. `a`/`b`/`c` are global slot offsets for most ops, and
/// signed jump offsets for `IF`/`IFNOT`/`GOTO`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Statement {
    pub op: Op,
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
    /// Field name -> entity-field cell offset, built once from `fielddefs` (first
    /// occurrence wins, matching the old linear `find_field`). Resolving a field by
    /// name is on the hot path of every `ent_get_*`/`ent_set_*` call (thousands per
    /// frame); the map makes it O(1) instead of an O(numfielddefs) string scan.
    field_ofs_map: std::collections::HashMap<String, u16>,
    /// Global name -> global cell offset, same rationale for `gget_*`/`gset_*`.
    global_ofs_map: std::collections::HashMap<String, u16>,
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

        // A hostile progs can carry an enormous (untrusted) record count; the
        // in-bounds precheck multiplies that count by the fixed record size via
        // `table_span` (a checked_mul) so an overflow becomes a clean parse
        // error instead of a tiny wrapped span that would slip past `slice_at`.
        let span = table_span;

        // --- statements ---
        let n = count(numstatements, "statement")?;
        let mut r = Reader::at(bytes, off(ofs_statements, "statements")?);
        let _ = r.slice_at(off(ofs_statements, "statements")?, span(n, STATEMENT_SIZE, "statement")?)?;
        let mut statements = Vec::with_capacity(n.min(bytes.len() / STATEMENT_SIZE));
        for _ in 0..n {
            statements.push(Statement {
                op: Op::from_code(r.u16()?),
                a: r.i16()?,
                b: r.i16()?,
                c: r.i16()?,
            });
        }

        // --- global defs ---
        let n = count(numglobaldefs, "globaldef")?;
        let mut r = Reader::at(bytes, off(ofs_globaldefs, "globaldefs")?);
        let _ = r.slice_at(off(ofs_globaldefs, "globaldefs")?, span(n, DEF_SIZE, "globaldef")?)?;
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
        let _ = r.slice_at(off(ofs_fielddefs, "fielddefs")?, span(n, DEF_SIZE, "fielddef")?)?;
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
        let _ = r.slice_at(off(ofs_functions, "functions")?, span(n, FUNCTION_SIZE, "function")?)?;
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
        let _ = r.slice_at(off(ofs_globals, "globals")?, span(ng, 4, "global")?)?;
        let mut globals = Vec::with_capacity(ng.min(bytes.len() / 4));
        for _ in 0..ng {
            globals.push(r.u32()?);
        }

        if entityfields < 0 {
            return Err(QError::invalid(format!(
                "progs.dat negative entityfields: {entityfields}"
            )));
        }

        // Build the name->offset caches once (first occurrence wins, matching the
        // old linear scans). Done here so every later lookup is O(1).
        let mut field_ofs_map = std::collections::HashMap::with_capacity(fielddefs.len());
        for d in &fielddefs {
            field_ofs_map
                .entry(string_in(&strings, d.s_name).to_string())
                .or_insert(d.ofs);
        }
        let mut global_ofs_map = std::collections::HashMap::with_capacity(globaldefs.len());
        for d in &globaldefs {
            global_ofs_map
                .entry(string_in(&strings, d.s_name).to_string())
                .or_insert(d.ofs);
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
            field_ofs_map,
            global_ofs_map,
        })
    }

    /// Entity-field cell offset for `name`, O(1) via the cached map (mirrors the
    /// first-match semantics of [`find_field`](Self::find_field)).
    pub fn field_offset(&self, name: &str) -> Option<u16> {
        self.field_ofs_map.get(name).copied()
    }

    /// Global cell offset for `name`, O(1) via the cached map.
    pub fn global_offset(&self, name: &str) -> Option<u16> {
        self.global_ofs_map.get(name).copied()
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
            let mn = st.op.mnemonic();
            let _ = writeln!(out, "  {s:5}: {mn:<10} a={} b={} c={}", st.a, st.b, st.c);
            if matches!(st.op, Op::Done | Op::Return) {
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
    fn table_span_rejects_overflow_not_panics() {
        // Normal counts multiply cleanly.
        assert_eq!(table_span(2, STATEMENT_SIZE, "statement").unwrap(), 16);
        assert_eq!(table_span(0, FUNCTION_SIZE, "function").unwrap(), 0);

        // A count whose product with the record size overflows `usize` must be
        // a clean parse error, never a panic and never a wrapped (tiny) span.
        // Pick a count that overflows on any pointer width: usize::MAX itself
        // times any record_size > 1 overflows.
        let huge = usize::MAX;
        let err = table_span(huge, STATEMENT_SIZE, "statement");
        assert!(err.is_err(), "overflowing span must be rejected");

        // And just under: half of MAX times 2 overflows (MAX is odd, so this is
        // MAX-1, *2 wraps).
        assert!(table_span(usize::MAX / 2 + 1, 2, "globaldef").is_err());

        // The largest non-overflowing span is accepted (boundary check).
        assert_eq!(table_span(usize::MAX, 1, "global").unwrap(), usize::MAX);
    }

    #[test]
    fn opcode_table_is_consistent() {
        // Round-trip every opcode through its ordinal.
        for v in 0..=OP_MAX {
            let op = Op::from_code(v);
            assert!(!matches!(op, Op::Invalid(_)), "opcode {v} is named");
            assert_eq!(op.code(), v);
        }
        assert_eq!(Op::from_code(OP_MAX + 1), Op::Invalid(OP_MAX + 1));
        assert_eq!(Op::Invalid(9999).code(), 9999);
        assert_eq!(Op::Invalid(9999).mnemonic(), "<bad op>");
        assert_eq!(Op::DivF.mnemonic(), "DIV_F");
        // Spot-check a few well-known ordinals against pr_comp.h.
        assert_eq!(Op::Done.code(), 0);
        assert_eq!(Op::AddF.code(), 6);
        assert_eq!(Op::Le.code(), 20);
        assert_eq!(Op::Address.code(), 30);
        assert_eq!(Op::Return.code(), 43);
        assert_eq!(Op::If.code(), 49);
        assert_eq!(Op::Call0.code(), 51);
        assert_eq!(Op::Goto.code(), 61);
        assert_eq!(Op::BitOr.code(), 65);
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
            Statement { op: Op::AddF, a: 28, b: 29, c: 30 },
            Statement { op: Op::Done, a: 0, b: 0, c: 0 },
        ];
        // a float global "x" at offset 28 (type 2 == ev_float)
        let globaldefs = [Def { type_: 2, ofs: 28, s_name: name_x }];
        let fielddefs: [Def; 0] = [];
        let functions = [func0, func];
        let globals: Vec<u32> = vec![0u32; 32];

        // layout: header, then sections in order
        fn ser_stmt(s: &Statement) -> Vec<u8> {
            let mut v = Vec::new();
            v.extend_from_slice(&s.op.code().to_le_bytes());
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
        assert_eq!(p.statements[0].op, Op::AddF);
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
