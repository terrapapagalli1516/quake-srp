//! End-to-end VM tests from outside the crate: hand-assemble a `progs.dat`
//! image, load it through the public API, execute it, and check results —
//! exercising the loader, interpreter, user-function calls, and builtins
//! together (the in-module tests cover each piece individually).

#![forbid(unsafe_code)]

use quake_rs::progs::{OFS_PARM0, OFS_RETURN};
use quake_rs::vm::Vm;

// opcode ordinals (pr_comp.h)
const DONE: u16 = 0;
const MUL_F: u16 = 1;
const ADD_F: u16 = 6;
const STORE_F: u16 = 31;
const STORE_S: u16 = 33;
const RETURN: u16 = 43;
const CALL1: u16 = 52;

const HEADER: usize = 60;

/// A tiny progs assembler.
struct Asm {
    strings: Vec<u8>,
    statements: Vec<[i32; 4]>, // op, a, b, c  (op in [0], stored as u16)
    functions: Vec<[i64; 9]>,  // first, parm_start, locals, profile, s_name, s_file, numparms, then parm0 size
    globals: Vec<u32>,
}

impl Asm {
    fn new(num_globals: usize) -> Self {
        Asm { strings: vec![0], statements: Vec::new(), functions: Vec::new(), globals: vec![0; num_globals] }
    }
    fn intern(&mut self, s: &str) -> i32 {
        let o = self.strings.len() as i32;
        self.strings.extend_from_slice(s.as_bytes());
        self.strings.push(0);
        o
    }
    fn set_f(&mut self, i: usize, v: f32) {
        self.globals[i] = v.to_bits();
    }
    fn set_i(&mut self, i: usize, v: i32) {
        self.globals[i] = v as u32;
    }
    fn stmt(&mut self, op: u16, a: i32, b: i32, c: i32) {
        self.statements.push([op as i32, a, b, c]);
    }
    /// first_statement<0 means builtin number -first.
    fn func(&mut self, first: i32, parm_start: i32, locals: i32, s_name: i32, numparms: i32, p0: u8) {
        self.functions.push([
            first as i64,
            parm_start as i64,
            locals as i64,
            0,
            s_name as i64,
            0,
            numparms as i64,
            p0 as i64,
            0,
        ]);
    }

    fn build(&self) -> Vec<u8> {
        let mut stmt_b = Vec::new();
        for s in &self.statements {
            stmt_b.extend_from_slice(&(s[0] as u16).to_le_bytes());
            stmt_b.extend_from_slice(&(s[1] as i16).to_le_bytes());
            stmt_b.extend_from_slice(&(s[2] as i16).to_le_bytes());
            stmt_b.extend_from_slice(&(s[3] as i16).to_le_bytes());
        }
        let mut func_b = Vec::new();
        for f in &self.functions {
            for &v in f.iter().take(7) {
                func_b.extend_from_slice(&(v as i32).to_le_bytes());
            }
            func_b.push(f[7] as u8);
            func_b.extend_from_slice(&[0u8; 7]); // remaining parm_size
        }
        let mut glob_b = Vec::new();
        for g in &self.globals {
            glob_b.extend_from_slice(&g.to_le_bytes());
        }

        let mut body = Vec::new();
        let ofs_statements = HEADER + body.len();
        body.extend_from_slice(&stmt_b);
        let ofs_globaldefs = HEADER + body.len(); // (empty)
        let ofs_fielddefs = HEADER + body.len(); // (empty)
        let ofs_functions = HEADER + body.len();
        body.extend_from_slice(&func_b);
        let ofs_strings = HEADER + body.len();
        body.extend_from_slice(&self.strings);
        let ofs_globals = HEADER + body.len();
        body.extend_from_slice(&glob_b);

        let header: [i32; 15] = [
            6,
            0,
            ofs_statements as i32,
            self.statements.len() as i32,
            ofs_globaldefs as i32,
            0,
            ofs_fielddefs as i32,
            0,
            ofs_functions as i32,
            self.functions.len() as i32,
            ofs_strings as i32,
            self.strings.len() as i32,
            ofs_globals as i32,
            self.globals.len() as i32,
            0,
        ];
        let mut out = Vec::new();
        for x in header {
            out.extend_from_slice(&x.to_le_bytes());
        }
        out.extend_from_slice(&body);
        out
    }
}

#[test]
fn add_two_floats_and_return() {
    let mut a = Asm::new(40);
    a.set_f(28, 3.0);
    a.set_f(29, 4.0);
    a.stmt(DONE, 0, 0, 0); // 0 reserved
    a.stmt(ADD_F, 28, 29, 30); // 1: g30 = 3 + 4
    a.stmt(RETURN, 30, 0, 0); // 2
    a.stmt(DONE, 0, 0, 0); // 3
    let s_main = a.intern("main");
    a.func(0, 0, 0, 0, 0, 0); // function 0 null
    a.func(1, 28, 0, s_main, 0, 0); // main @ stmt 1

    let img = a.build();
    let mut vm = Vm::load(&img).expect("load progs");
    vm.call_by_name("main").expect("run main");
    assert_eq!(vm.gf(OFS_RETURN), 7.0);
}

#[test]
fn user_call_plus_builtins_end_to_end() {
    // main: result = double(6*... ) ; then dprint(ftos(result)). Mirrors gen_progs.py.
    let mut a = Asm::new(40);
    // globals
    let (g_result, g_c2, g_x, g_tmp, g_c21, g_double, g_ftos, g_dprint) =
        (30usize, 33usize, 34usize, 35usize, 36usize, 37usize, 38usize, 39usize);
    a.set_f(g_c2, 2.0);
    a.set_f(g_c21, 21.0);
    a.set_i(g_double, 2);
    a.set_i(g_ftos, 3);
    a.set_i(g_dprint, 4);

    let pi = |x: usize| x as i32;
    // statements: [0] reserved; main [1..9]; double [10..11]
    a.stmt(DONE, 0, 0, 0); // 0
    a.stmt(STORE_F, pi(g_c21), OFS_PARM0 as i32, 0); // 1 parm0 = 21
    a.stmt(CALL1, pi(g_double), 0, 0); // 2 double(21) -> RETURN
    a.stmt(STORE_F, OFS_RETURN as i32, pi(g_result), 0); // 3 result = RETURN
    a.stmt(STORE_F, pi(g_result), OFS_PARM0 as i32, 0); // 4 parm0 = result
    a.stmt(CALL1, pi(g_ftos), 0, 0); // 5 ftos -> RETURN = "42"
    a.stmt(STORE_S, OFS_RETURN as i32, OFS_PARM0 as i32, 0); // 6 parm0 = string
    a.stmt(CALL1, pi(g_dprint), 0, 0); // 7 dprint("42")
    a.stmt(RETURN, pi(g_result), 0, 0); // 8 return result
    a.stmt(DONE, 0, 0, 0); // 9
    a.stmt(MUL_F, pi(g_x), pi(g_c2), pi(g_tmp)); // 10 double: tmp = x*2
    a.stmt(RETURN, pi(g_tmp), 0, 0); // 11

    let (s_main, s_double, s_ftos, s_dprint) =
        (a.intern("main"), a.intern("double"), a.intern("ftos"), a.intern("dprint"));
    a.func(0, 0, 0, 0, 0, 0); // 0 null
    a.func(1, 28, 0, s_main, 0, 0); // 1 main @ 1
    a.func(10, pi(g_x), 2, s_double, 1, 1); // 2 double @ 10, 1 parm, 2 locals
    a.func(-26, 0, 0, s_ftos, 0, 0); // 3 ftos = builtin 26
    a.func(-25, 0, 0, s_dprint, 0, 0); // 4 dprint = builtin 25

    let img = a.build();
    let mut vm = Vm::load(&img).expect("load progs");
    vm.call_by_name("main").expect("run main");
    assert_eq!(vm.gf(OFS_RETURN), 42.0, "double(21) should be 42");
    assert_eq!(vm.output().trim(), "42", "dprint(ftos(42)) should output 42");
}

#[test]
fn calling_unknown_function_is_an_error_not_a_panic() {
    let mut a = Asm::new(32);
    a.stmt(DONE, 0, 0, 0);
    a.func(0, 0, 0, 0, 0, 0);
    let img = a.build();
    let mut vm = Vm::load(&img).unwrap();
    assert!(vm.call_by_name("nonexistent").is_err());
}
