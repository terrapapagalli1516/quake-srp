#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Symbolic QuakeC disassembly + a builtin-call census of a progs.dat.

`quaketool dis` prints raw global numbers; this resolves them to def names and
constant values, resolves CALL targets to function names, and — the census
part — records, for every call site, which function makes it and the constant
arguments it passes (a string immediate, a float, a vector). That answers
questions like "every stuffcmd string the id1 progs send", "every sample the
progs hand to sound()", "which builtins are ever called" without the QuakeC
source (which is not on disk).

    uv run census/qcsym.py <progs.dat | pak0.pak> dis            # full listing
    uv run census/qcsym.py <progs.dat | pak0.pak> calls [builtin ...]
    uv run census/qcsym.py <progs.dat | pak0.pak> builtins       # builtins called + site counts
    uv run census/qcsym.py <progs.dat | pak0.pak> strings        # string immediates by use
    uv run census/qcsym.py <progs.dat | pak0.pak> func NAME      # one function

Argument tracking is intra-function and linear: the last STORE into
OFS_PARM0+3*i before a CALLn is taken as argument i. That is how qcc emits
calls (it evaluates each argument into its parm slot right before the call),
so it is exact for constants; a non-constant argument prints as its def name
(e.g. `self.noise` shows as the temp it was loaded into).
"""

import struct
import sys
from collections import Counter, defaultdict

OPS = [
    "DONE", "MUL_F", "MUL_V", "MUL_FV", "MUL_VF", "DIV_F", "ADD_F", "ADD_V", "SUB_F", "SUB_V",
    "EQ_F", "EQ_V", "EQ_S", "EQ_E", "EQ_FNC", "NE_F", "NE_V", "NE_S", "NE_E", "NE_FNC",
    "LE", "GE", "LT", "GT", "LOAD_F", "LOAD_V", "LOAD_S", "LOAD_ENT", "LOAD_FLD", "LOAD_FNC",
    "ADDRESS", "STORE_F", "STORE_V", "STORE_S", "STORE_ENT", "STORE_FLD", "STORE_FNC",
    "STOREP_F", "STOREP_V", "STOREP_S", "STOREP_ENT", "STOREP_FLD", "STOREP_FNC", "RETURN",
    "NOT_F", "NOT_V", "NOT_S", "NOT_ENT", "NOT_FNC", "IF", "IFNOT", "CALL0", "CALL1", "CALL2",
    "CALL3", "CALL4", "CALL5", "CALL6", "CALL7", "CALL8", "STATE", "GOTO", "AND", "OR",
    "BITAND", "BITOR",
]
TYPES = ["void", "string", "float", "vector", "entity", "field", "function", "pointer"]
DEF_SAVEGLOBAL = 1 << 15
OFS_PARM0 = 4


def load_progs(path):
    data = open(path, "rb").read()
    if data[:4] == b"PACK":
        dirofs, dirlen = struct.unpack_from("<ii", data, 4)
        for i in range(dirlen // 64):
            name, fofs, flen = struct.unpack_from("<56sii", data, dirofs + i * 64)
            if name.split(b"\0")[0] == b"progs.dat":
                return data[fofs:fofs + flen]
        sys.exit("no progs.dat in pak")
    return data


class Progs:
    def __init__(self, data):
        h = struct.unpack_from("<15i", data, 0)
        (self.version, self.crc, ofs_st, n_st, ofs_gd, n_gd, ofs_fd, n_fd, ofs_fn, n_fn,
         ofs_str, n_str, ofs_gl, n_gl, self.entityfields) = h
        self.strings = data[ofs_str:ofs_str + n_str]
        self.statements = [struct.unpack_from("<Hhhh", data, ofs_st + 8 * i) for i in range(n_st)]
        self.globaldefs = [struct.unpack_from("<HHi", data, ofs_gd + 8 * i) for i in range(n_gd)]
        self.fielddefs = [struct.unpack_from("<HHi", data, ofs_fd + 8 * i) for i in range(n_fd)]
        self.functions = []
        for i in range(n_fn):
            f = struct.unpack_from("<7i8B", data, ofs_fn + 36 * i)
            self.functions.append({
                "first": f[0], "parm_start": f[1], "locals": f[2], "name": self.s(f[4]),
                "file": self.s(f[5]), "numparms": f[6],
            })
        self.gbytes = data[ofs_gl:ofs_gl + 4 * n_gl]
        self.nglobals = n_gl
        # global offset -> (name, type)
        self.gname = {}
        for t, ofs, sn in self.globaldefs:
            name = self.s(sn)
            ty = TYPES[t & ~DEF_SAVEGLOBAL & 7] if (t & ~DEF_SAVEGLOBAL) < 8 else f"t{t}"
            if ofs not in self.gname or self.gname[ofs][0] in ("", "IMMEDIATE"):
                self.gname[ofs] = (name, ty)
            if ty == "vector":
                for k, sfx in enumerate("_x _y _z".split()):
                    self.gname.setdefault(ofs + k, (name + sfx, "float"))
        self.fname = {}
        for t, ofs, sn in self.fielddefs:
            self.fname.setdefault(ofs, self.s(sn))
        # which statement range belongs to which function
        starts = sorted((f["first"], i) for i, f in enumerate(self.functions) if f["first"] > 0)
        self.func_of_stmt = {}
        for k, (first, fi) in enumerate(starts):
            end = starts[k + 1][0] if k + 1 < len(starts) else len(self.statements)
            for s in range(first, end):
                self.func_of_stmt[s] = fi
        self.bodies = {fi: (first, starts[k + 1][0] if k + 1 < len(starts) else len(self.statements))
                       for k, (first, fi) in enumerate(starts)}

    def s(self, ofs):
        if ofs < 0 or ofs >= len(self.strings):
            return f"<badstr {ofs}>"
        end = self.strings.index(b"\0", ofs)
        return self.strings[ofs:end].decode("latin-1")

    def gi(self, o):
        return struct.unpack_from("<i", self.gbytes, 4 * o)[0]

    def gf(self, o):
        return struct.unpack_from("<f", self.gbytes, 4 * o)[0]

    def is_const(self, o):
        n = self.gname.get(o)
        return n is None or n[0] in ("IMMEDIATE", "")

    def const(self, o, ty):
        """Render global o as a constant of type ty (string/float/vector/function/field)."""
        if ty == "string":
            return repr(self.s(self.gi(o)))
        if ty == "vector":
            return "'%g %g %g'" % (self.gf(o), self.gf(o + 1), self.gf(o + 2))
        if ty == "function":
            fi = self.gi(o)
            return self.functions[fi]["name"] if 0 <= fi < len(self.functions) else f"fn#{fi}"
        if ty == "field":
            return "." + self.fname.get(self.gi(o), f"fld{self.gi(o)}")
        if ty == "entity":
            return f"ent#{self.gi(o)}"
        return "%g" % self.gf(o)

    def operand(self, o, ty=None):
        n = self.gname.get(o)
        if n and n[0] not in ("IMMEDIATE", ""):
            name, gty = n
            if gty == "function" and self.gi(o) and o >= 28:
                return name
            return name
        if o < 28:
            return {1: "RETURN", 4: "PARM0", 7: "PARM1", 10: "PARM2", 13: "PARM3", 16: "PARM4",
                    19: "PARM5", 22: "PARM6", 25: "PARM7"}.get(o, f"g{o}")
        if n and n[0] == "IMMEDIATE":
            return self.const(o, n[1])  # an immediate carries its own type
        return f"t{o}"  # a temporary (qcc gives temps no def)


STORE_TY = {"STORE_F": "float", "STORE_V": "vector", "STORE_S": "string", "STORE_ENT": "entity",
            "STORE_FLD": "field", "STORE_FNC": "function"}
ARG_TY = {"string": "string", "vector": "vector", "float": "float", "entity": "entity",
          "field": "field", "function": "function"}


def call_sites(p):
    """Yield (caller, callee, [arg strings], stmt) for every CALLn."""
    for fi, (first, end) in p.bodies.items():
        parms = {}
        for si in range(first, end):
            op, a, b, c = p.statements[si]
            name = OPS[op] if op < len(OPS) else f"op{op}"
            if name in STORE_TY and OFS_PARM0 <= b < OFS_PARM0 + 24 and (b - OFS_PARM0) % 3 == 0:
                ty = STORE_TY[name]
                parms[(b - OFS_PARM0) // 3] = p.operand(a, ty) if p.is_const(a) else p.operand(a, ty)
            elif name.startswith("CALL"):
                n = int(name[4:])
                callee_g = a
                if p.is_const(callee_g) or p.gname.get(callee_g, ("", ""))[1] == "function":
                    fnum = p.gi(callee_g)
                    callee = p.functions[fnum]["name"] if 0 < fnum < len(p.functions) else p.operand(a)
                    # a function-typed *variable* (e.g. self.th_die loaded into a temp)
                    if p.gname.get(callee_g, ("", ""))[0] not in ("", "IMMEDIATE") and \
                            p.gname[callee_g][0] != callee:
                        callee = p.gname[callee_g][0] if not p.gname[callee_g][0].startswith("_") else callee
                else:
                    callee = p.operand(a)
                yield p.functions[fi]["name"], callee, [parms.get(i, "?") for i in range(n)], si
                parms = {}


def builtin_names(p):
    return {f["name"]: -f["first"] for f in p.functions if f["first"] < 0}


def fmt_stmt(p, si):
    op, a, b, c = p.statements[si]
    name = OPS[op] if op < len(OPS) else f"op{op}"
    if name in ("GOTO",):
        return f"{si:6d}: {name:10s} -> {si + a}"
    if name in ("IF", "IFNOT"):
        return f"{si:6d}: {name:10s} {p.operand(a)} -> {si + b}"
    if name.startswith("CALL"):
        fnum = p.gi(a)
        tgt = p.gname.get(a, ("", ""))[0]
        if tgt in ("", "IMMEDIATE") and 0 < fnum < len(p.functions):
            tgt = p.functions[fnum]["name"]
        return f"{si:6d}: {name:10s} {tgt}"
    ty = STORE_TY.get(name)
    if ty:
        return f"{si:6d}: {name:10s} {p.operand(a, ty)} -> {p.operand(b)}"
    if name.startswith("STOREP"):
        t = {"STOREP_S": "string", "STOREP_V": "vector", "STOREP_FNC": "function",
             "STOREP_FLD": "field"}.get(name, "float")
        return f"{si:6d}: {name:10s} {p.operand(a, t)} -> *{p.operand(b)}"
    if name in ("LOAD_F", "LOAD_V", "LOAD_S", "LOAD_ENT", "LOAD_FLD", "LOAD_FNC", "ADDRESS"):
        return f"{si:6d}: {name:10s} {p.operand(a)}.{p.const(b, 'field')[1:]} -> {p.operand(c)}"
    if name == "STATE":
        return f"{si:6d}: STATE      frame={p.operand(a)} think={p.operand(b, 'function')}"
    if name in ("RETURN",):
        return f"{si:6d}: RETURN     {p.operand(a)}"
    if name == "DONE":
        return f"{si:6d}: DONE"
    tys = "float"
    if name.endswith("_V"):
        tys = "vector"
    if name.endswith("_S"):
        tys = "string"
    return f"{si:6d}: {name:10s} {p.operand(a, tys)}, {p.operand(b, tys)} -> {p.operand(c)}"


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        sys.exit(2)
    p = Progs(load_progs(sys.argv[1]))
    cmd, args = sys.argv[2], sys.argv[3:]
    bnames = builtin_names(p)
    if cmd == "dis":
        for fi, (first, end) in sorted(p.bodies.items(), key=lambda kv: kv[1][0]):
            f = p.functions[fi]
            print(f"\nfunction {f['name']}  // {f['file']}  parms={f['numparms']}")
            for si in range(first, end):
                print(fmt_stmt(p, si))
    elif cmd == "func":
        for fi, (first, end) in p.bodies.items():
            if p.functions[fi]["name"] in args:
                f = p.functions[fi]
                print(f"function {f['name']}  // {f['file']}  parms={f['numparms']}")
                for si in range(first, end):
                    print(fmt_stmt(p, si))
    elif cmd == "calls":
        want = set(args)
        for caller, callee, cargs, si in call_sites(p):
            if not want or callee in want:
                print(f"{caller:32s} {callee}({', '.join(cargs)})")
    elif cmd == "builtins":
        cnt = Counter()
        callers = defaultdict(set)
        for caller, callee, cargs, si in call_sites(p):
            if callee in bnames:
                cnt[callee] += 1
                callers[callee].add(caller)
        for name, num in sorted(bnames.items(), key=lambda kv: kv[1]):
            print(f"#{num:<3d} {name:18s} sites={cnt[name]:<4d} callers={len(callers[name])}")
    elif cmd == "strings":
        for caller, callee, cargs, si in call_sites(p):
            for i, a in enumerate(cargs):
                if a.startswith("'") and not a.startswith("'" + " ") and (a.endswith("'") or a.endswith('"')):
                    if not a[1:2].isdigit() and not a[1:2] == "-":
                        print(f"{callee}[{i}]\t{a}\t{caller}")
    else:
        sys.exit(f"unknown command {cmd}")


if __name__ == "__main__":
    main()
