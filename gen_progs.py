#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Hand-assemble a tiny QuakeC `progs.dat` (bytecode v6) to exercise the Rust VM
end-to-end: main() calls a user function double(21) -> 42, then dprint(ftos(42)).
This is an independent assembler (in Python) of the format the Rust VM executes."""

import struct, sys, os

OUT = sys.argv[1] if len(sys.argv) > 1 else "samples/test.dat"
os.makedirs(os.path.dirname(OUT) or ".", exist_ok=True)

# reserved global offsets
OFS_RETURN, OFS_PARM0 = 1, 4
# opcodes (ordinals from pr_comp.h)
DONE, MUL_F, STORE_F, STORE_S, RETURN, CALL1 = 0, 1, 31, 33, 43, 52

# ---- string heap ----
strings = bytearray(b"\x00")  # string 0 = ""
def intern(s):
    o = len(strings); strings.extend(s.encode() + b"\x00"); return o
s_main, s_double, s_ftos, s_dprint = (intern(x) for x in ("main", "double", "ftos", "dprint"))

# ---- globals (40 cells) ----
NG = 40
gb = bytearray(NG * 4)
def set_f(i, v): struct.pack_into("<f", gb, i*4, v)
def set_i(i, v): struct.pack_into("<i", gb, i*4, v)
# layout
G_RESULT, G_C2, G_X, G_TMP, G_C21, G_DOUBLE, G_FTOS, G_DPRINT = 30, 33, 34, 35, 36, 37, 38, 39
set_f(G_C2, 2.0)
set_f(G_C21, 21.0)
set_i(G_DOUBLE, 2)   # function index of `double`
set_i(G_FTOS, 3)     # function index of `ftos` builtin record
set_i(G_DPRINT, 4)   # function index of `dprint` builtin record

# ---- statements ----
# [0] reserved DONE; main = [1..9]; double = [10..11]
statements = [
    (DONE, 0, 0, 0),                       # 0  reserved (statement 0 is never run)
    (STORE_F, G_C21, OFS_PARM0, 0),        # 1  parm0 = 21.0
    (CALL1, G_DOUBLE, 0, 0),               # 2  double(21) -> RETURN = 42
    (STORE_F, OFS_RETURN, G_RESULT, 0),    # 3  result = RETURN
    (STORE_F, G_RESULT, OFS_PARM0, 0),     # 4  parm0 = result
    (CALL1, G_FTOS, 0, 0),                 # 5  ftos(42) -> RETURN = "42"
    (STORE_S, OFS_RETURN, OFS_PARM0, 0),   # 6  parm0 = the string
    (CALL1, G_DPRINT, 0, 0),               # 7  dprint("42")
    (RETURN, G_RESULT, 0, 0),              # 8  return result (42)
    (DONE, 0, 0, 0),                       # 9
    (MUL_F, G_X, G_C2, G_TMP),             # 10 double: tmp = x * 2
    (RETURN, G_TMP, 0, 0),                 # 11 return tmp
]

# ---- functions ----  (first_statement<0 => builtin number)
def func(first, parm_start, locals_, s_name, numparms, parm_size):
    ps = list(parm_size) + [0] * (8 - len(parm_size))
    return struct.pack("<7i", first, parm_start, locals_, 0, s_name, 0, numparms) + bytes(ps)
functions = [
    func(0, 0, 0, 0, 0, []),               # 0  null
    func(1, 28, 0, s_main, 0, []),         # 1  main @ stmt 1
    func(10, G_X, 2, s_double, 1, [1]),    # 2  double @ stmt 10, parm_start=G_X, locals=2 (x,tmp)
    func(-26, 0, 0, s_ftos, 0, []),        # 3  ftos    = builtin #26
    func(-25, 0, 0, s_dprint, 0, []),      # 4  dprint  = builtin #25
]

# ---- serialize ----
def ser_stmts():
    b = bytearray()
    for op, a, c, d in statements:
        b += struct.pack("<Hhhh", op, a, c, d)
    return bytes(b)

stmt_bytes = ser_stmts()
func_bytes = b"".join(functions)
globaldefs = b""  # not needed for execution
fielddefs = b""

HEADER = 60
body = bytearray()
ofs_statements = HEADER + len(body); body += stmt_bytes
ofs_globaldefs = HEADER + len(body); body += globaldefs
ofs_fielddefs = HEADER + len(body); body += fielddefs
ofs_functions = HEADER + len(body); body += func_bytes
ofs_strings = HEADER + len(body); body += strings
ofs_globals = HEADER + len(body); body += gb

header = struct.pack(
    "<15i",
    6, 0,
    ofs_statements, len(statements),
    ofs_globaldefs, 0,
    ofs_fielddefs, 0,
    ofs_functions, len(functions),
    ofs_strings, len(strings),
    ofs_globals, NG,
    0,  # entityfields
)

with open(OUT, "wb") as f:
    f.write(header + bytes(body))
print(f"wrote {OUT}: {len(header)+len(body)} bytes, {len(statements)} statements, {len(functions)} functions")
