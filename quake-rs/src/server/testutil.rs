//! Shared test fixtures for the `server` submodules' unit tests.
//!
//! Test-only (`#[cfg(test)]`). A synthetic `progs.dat` builder (the same
//! serializer shape as `vm.rs`'s tests), a handful of hand-built BSPs (empty,
//! one open leaf, a flat floor) and the synthetic QuakeC programs several
//! modules' tests share (marker spawn, touch, player, attack, changelevel).
//! Fixtures used by only one module's tests live next to those tests instead.

use crate::bsp::Bsp;
use crate::progs::{Def, Function, MAX_PARMS, OFS_PARM0, Op, PROG_VERSION, RESERVED_OFS, Statement};
use crate::server::{NUM_SPAWN_PARMS, Server, parm_global_name};

const HEADER_SIZE: usize = 60;

// ---- synthetic progs.dat builder (mirrors vm.rs's test serializer) ----

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

pub(crate) struct Builder {
    strings: Vec<u8>,
    statements: Vec<Statement>,
    globaldefs: Vec<Def>,
    fielddefs: Vec<Def>,
    functions: Vec<Function>,
    nglobals: usize,
    pub(crate) entityfields: i32,
}

impl Builder {
    pub(crate) fn new() -> Builder {
        Builder {
            strings: vec![0u8],
            statements: Vec::new(),
            globaldefs: Vec::new(),
            fielddefs: Vec::new(),
            functions: vec![Function {
                first_statement: 0,
                parm_start: 0,
                locals: 0,
                profile: 0,
                s_name: 0,
                s_file: 0,
                numparms: 0,
                parm_size: [0; MAX_PARMS],
            }],
            nglobals: 128,
            entityfields: 0,
        }
    }
    pub(crate) fn intern(&mut self, s: &str) -> i32 {
        let ofs = self.strings.len() as i32;
        self.strings.extend_from_slice(s.as_bytes());
        self.strings.push(0);
        ofs
    }
    /// Add a global def of `type_` at `ofs` named `name`.
    pub(crate) fn add_global(&mut self, name: &str, type_: u16, ofs: u16) {
        let s = self.intern(name);
        self.globaldefs.push(Def { type_, ofs, s_name: s });
    }
    /// Add a field def of `type_` at `ofs` named `name`.
    pub(crate) fn add_field(&mut self, name: &str, type_: u16, ofs: u16) {
        let s = self.intern(name);
        self.fielddefs.push(Def { type_, ofs, s_name: s });
    }
    /// Add a bytecode function `name` with `stmts`; returns its index.
    pub(crate) fn add_function(&mut self, name: &str, stmts: Vec<Statement>) -> usize {
        let first = self.statements.len() as i32;
        let s_name = self.intern(name);
        self.statements.extend(stmts);
        self.functions.push(Function {
            first_statement: first,
            parm_start: RESERVED_OFS as i32,
            locals: 0,
            profile: 0,
            s_name,
            s_file: 0,
            numparms: 0,
            parm_size: [0; MAX_PARMS],
        });
        self.functions.len() - 1
    }
    /// Add a builtin function record (`first_statement = -builtin_num`) named
    /// `name`, so QuakeC can `CALL` into the engine builtin table; returns its
    /// function index.
    pub(crate) fn add_builtin(&mut self, name: &str, builtin_num: i32) -> usize {
        let s_name = self.intern(name);
        self.functions.push(Function {
            first_statement: -builtin_num,
            parm_start: RESERVED_OFS as i32,
            locals: 0,
            profile: 0,
            s_name,
            s_file: 0,
            numparms: 0,
            parm_size: [0; MAX_PARMS],
        });
        self.functions.len() - 1
    }
    pub(crate) fn build(&self) -> Vec<u8> {
        let globals: Vec<u32> = vec![0u32; self.nglobals];
        let mut body = Vec::new();
        let ofs_statements = HEADER_SIZE + body.len();
        for s in &self.statements {
            body.extend_from_slice(&ser_stmt(s));
        }
        let ofs_globaldefs = HEADER_SIZE + body.len();
        for d in &self.globaldefs {
            body.extend_from_slice(&ser_def(d));
        }
        let ofs_fielddefs = HEADER_SIZE + body.len();
        for d in &self.fielddefs {
            body.extend_from_slice(&ser_def(d));
        }
        let ofs_functions = HEADER_SIZE + body.len();
        for f in &self.functions {
            body.extend_from_slice(&ser_func(f));
        }
        let ofs_strings = HEADER_SIZE + body.len();
        body.extend_from_slice(&self.strings);
        let ofs_globals = HEADER_SIZE + body.len();
        for g in &globals {
            body.extend_from_slice(&g.to_le_bytes());
        }
        let header: [i32; 15] = [
            PROG_VERSION,
            0,
            ofs_statements as i32,
            self.statements.len() as i32,
            ofs_globaldefs as i32,
            self.globaldefs.len() as i32,
            ofs_fielddefs as i32,
            self.fielddefs.len() as i32,
            ofs_functions as i32,
            self.functions.len() as i32,
            ofs_strings as i32,
            self.strings.len() as i32,
            ofs_globals as i32,
            globals.len() as i32,
            self.entityfields,
        ];
        let mut out = Vec::new();
        for x in header {
            out.extend_from_slice(&x.to_le_bytes());
        }
        out.extend_from_slice(&body);
        out
    }
}

/// An empty BSP (no geometry); world queries are total and report SOLID.
pub(crate) fn empty_bsp() -> Bsp {
    Bsp {
        version: crate::bsp::BSPVERSION,
        entities: String::new(),
        planes: Vec::new(),
        vertexes: Vec::new(),
        edges: Vec::new(),
        faces: Vec::new(),
        nodes: Vec::new(),
        leafs: Vec::new(),
        clipnodes: Vec::new(),
        texinfo: Vec::new(),
        models: Vec::new(),
        marksurfaces: Vec::new(),
        surfedges: Vec::new(),
        textures: Vec::new(),
        visibility: Vec::new(),
        lighting: Vec::new(),
    }
}

/// A BSP carrying a specific entity text blob.
pub(crate) fn bsp_with_entities(text: &str) -> Bsp {
    let mut b = empty_bsp();
    b.entities = text.to_string();
    b
}

// ev_* type codes (etype_t ordinals; see progs::EType).
pub(crate) const EV_STRING: u16 = 1;
pub(crate) const EV_FLOAT: u16 = 2;
pub(crate) const EV_FUNCTION: u16 = 6;

pub(crate) const EV_VECTOR: u16 = 3;
pub(crate) const EV_ENTITY: u16 = 4;

/// A BSP whose world model is a single empty leaf, so a world box-trace runs
/// clear (fraction 1) instead of the empty-BSP "everything solid". Hull 0's
/// headnode (0) names node 0, whose children are the empty leaf -> CONTENTS
/// EMPTY. This lets the entity-clip tests see entity collisions instead of a
/// world block at fraction 0.
pub(crate) fn world_open_bsp() -> Bsp {
    use crate::bsp::{CONTENTS_EMPTY, CONTENTS_SOLID, DClipNode, DLeaf, DModel, DNode, DPlane};
    let mut b = empty_bsp();
    // One axial plane at x = -100000 (far away), so every test point is on
    // its front side -> child 0 -> the empty leaf.
    b.planes = vec![DPlane { normal: [1.0, 0.0, 0.0], dist: -100000.0, ptype: 0 }];
    // node 0: both children name leaf 1 (index -(- ( -2)) ...). Children are
    // i16: a negative child -(leaf)-1. Leaf 1 -> child = -(1)-1 = -2.
    b.nodes = vec![DNode {
        planenum: 0,
        children: [-2, -2], // both sides -> leaf 1 (CONTENTS_EMPTY)
        mins: [0; 3],
        maxs: [0; 3],
        firstface: 0,
        numfaces: 0,
    }];
    // leaf 0 is the solid leaf; leaf 1 is empty open space.
    b.leafs = vec![
        DLeaf {
            contents: CONTENTS_SOLID,
            visofs: -1,
            mins: [0; 3],
            maxs: [0; 3],
            firstmarksurface: 0,
            nummarksurfaces: 0,
            ambient_level: [0; 4],
        },
        DLeaf {
            contents: CONTENTS_EMPTY,
            visofs: -1,
            mins: [0; 3],
            maxs: [0; 3],
            firstmarksurface: 0,
            nummarksurfaces: 0,
            ambient_level: [0; 4],
        },
    ];
    // Clip hulls 1/2: a single clipnode that is empty on both sides.
    b.clipnodes = vec![DClipNode { planenum: 0, children: [CONTENTS_EMPTY as i16, CONTENTS_EMPTY as i16] }];
    b.models = vec![DModel {
        mins: [-4096.0; 3],
        maxs: [4096.0; 3],
        origin: [0.0; 3],
        headnode: [0, 0, 0, 0],
        visleafs: 1,
        firstface: 0,
        numfaces: 0,
    }];
    b
}

/// Build a progs whose "marker" classname spawn function sets a global float
/// `spawned_flag` to 1.0, so we can prove the spawner executed it. Also adds
/// a "classname" string field and "spawnflags"/"think"/"nextthink" fields.
pub(crate) fn marker_progs() -> (Vec<u8>, usize, usize) {
    let mut b = Builder::new();
    b.entityfields = 8;

    // Globals: a float "spawned_flag" at offset 30, plus the well-known
    // self/other/time/world globals the server sets.
    let g_flag = 30u16;
    b.add_global("spawned_flag", EV_FLOAT, g_flag);
    b.add_global("self", 4 /*ev_entity*/, 31);
    b.add_global("other", 4, 32);
    b.add_global("time", EV_FLOAT, 33);
    b.add_global("world", 4, 34);
    b.add_global("frametime", EV_FLOAT, 35);

    // Fields: classname(string)@1, spawnflags(float)@2, think(function)@3,
    // nextthink(float)@4, frame(float)@5, origin(vector)@5? keep simple.
    b.add_field("classname", EV_STRING, 1);
    b.add_field("spawnflags", EV_FLOAT, 2);
    b.add_field("think", EV_FUNCTION, 3);
    b.add_field("nextthink", EV_FLOAT, 4);

    // Spawn function "marker": STORE_F const(1.0) -> spawned_flag; DONE.
    // We need a global holding 1.0; put it at offset 40 and set it after load.
    let g_one = 40u16;
    let marker = b.add_function(
        "marker",
        vec![
            Statement { op: Op::StoreF, a: g_one as i16, b: g_flag as i16, c: 0 },
            Statement { op: Op::Done, a: 0, b: 0, c: 0 },
        ],
    );

    let img = b.build();
    (img, marker, g_one as usize)
}

/// Build a progs whose "do_touch" function stores the constant at `g_one`
/// into the global `touched_flag` (offset `g_flag`). Field/global layout is
/// shared by the sv_move and touch_triggers tests so a synthetic Server can
/// place SOLID_BBOX / SOLID_TRIGGER edicts and run them. Returns
/// `(image, touch_fn_index, g_one_offset, g_flag_offset)`.
pub(crate) fn touch_progs() -> (Vec<u8>, usize, usize, usize) {
    let mut b = Builder::new();
    b.entityfields = 32;

    // Globals.
    let g_flag = 30u16;
    b.add_global("touched_flag", EV_FLOAT, g_flag);
    b.add_global("self", EV_ENTITY, 31);
    b.add_global("other", EV_ENTITY, 32);
    b.add_global("time", EV_FLOAT, 33);
    b.add_global("world", EV_ENTITY, 34);
    b.add_global("frametime", EV_FLOAT, 35);
    b.add_global("force_retouch", EV_FLOAT, 36);
    let g_one = 40u16;

    // Fields the move/touch code reads.
    b.add_field("classname", EV_STRING, 1);
    b.add_field("solid", EV_FLOAT, 2);
    b.add_field("touch", EV_FUNCTION, 3);
    b.add_field("origin", EV_VECTOR, 4); // 4,5,6
    b.add_field("mins", EV_VECTOR, 7); // 7,8,9
    b.add_field("maxs", EV_VECTOR, 10); // 10,11,12
    b.add_field("absmin", EV_VECTOR, 13); // 13,14,15
    b.add_field("absmax", EV_VECTOR, 16); // 16,17,18
    b.add_field("model", EV_STRING, 19);
    b.add_field("movetype", EV_FLOAT, 20);
    b.add_field("nextthink", EV_FLOAT, 21);
    b.add_field("flags", EV_FLOAT, 22);
    b.add_field("velocity", EV_VECTOR, 23); // 23,24,25
    b.add_field("size", EV_VECTOR, 26); // 26,27,28
    b.add_field("groundentity", EV_ENTITY, 29);
    b.add_field("owner", EV_ENTITY, 30); // SV_ClipToLinks owner-skip tests
    b.add_field("modelindex", EV_FLOAT, 31); // Solid::Bsp hull resolution tests

    let touch_fn = b.add_function(
        "do_touch",
        vec![
            Statement { op: Op::StoreF, a: g_one as i16, b: g_flag as i16, c: 0 },
            Statement { op: Op::Done, a: 0, b: 0, c: 0 },
        ],
    );

    let img = b.build();
    (img, touch_fn, g_one as usize, g_flag as usize)
}

/// A BSP with a flat floor at `z = 0`: the half-space `z >= 0` is open
/// (`CONTENTS_EMPTY`) and `z < 0` is solid (`CONTENTS_SOLID`), in every hull.
/// A player box dropped onto it lands on `z = 0` and cannot tunnel through.
/// The split plane is the axial +Z plane at `dist = 0` (`ptype = 2`).
pub(crate) fn floor_bsp() -> Bsp {
    use crate::bsp::{CONTENTS_EMPTY, CONTENTS_SOLID, DClipNode, DLeaf, DModel, DNode, DPlane};
    let mut b = empty_bsp();
    // plane 0: +Z at z = 0 (the point hull, hull 0). plane 1: +Z at z = 24,
    // which models how the BSP compiler bakes the player box (mins.z = -24)
    // into hull 1 — so a *point* traced against hull 1 stops with the box
    // bottom resting on the real floor at z = 0 (origin.z = 24).
    b.planes = vec![
        DPlane { normal: [0.0, 0.0, 1.0], dist: 0.0, ptype: 2 },
        DPlane { normal: [0.0, 0.0, 1.0], dist: 24.0, ptype: 2 },
    ];
    // Hull-0 node: front side (z >= 0, child 0) -> empty leaf 1;
    // back side (z < 0, child 1) -> solid leaf 0. Negative child -(leaf)-1:
    // leaf 1 -> -2 (empty), leaf 0 -> -1 (solid).
    b.nodes = vec![DNode { planenum: 0, children: [-2, -1], mins: [0; 3], maxs: [0; 3], firstface: 0, numfaces: 0 }];
    b.leafs = vec![
        DLeaf {
            contents: CONTENTS_SOLID,
            visofs: -1,
            mins: [0; 3],
            maxs: [0; 3],
            firstmarksurface: 0,
            nummarksurfaces: 0,
            ambient_level: [0; 4],
        },
        DLeaf {
            contents: CONTENTS_EMPTY,
            visofs: -1,
            mins: [0; 3],
            maxs: [0; 3],
            firstmarksurface: 0,
            nummarksurfaces: 0,
            ambient_level: [0; 4],
        },
    ];
    // Clip hulls 1/2 split on plane 1 (z = 24): above empty, below solid —
    // the player-expanded floor.
    b.clipnodes = vec![DClipNode { planenum: 1, children: [CONTENTS_EMPTY as i16, CONTENTS_SOLID as i16] }];
    b.models = vec![DModel {
        mins: [-4096.0; 3],
        maxs: [4096.0; 3],
        origin: [0.0; 3],
        headnode: [0, 0, 0, 0],
        visleafs: 1,
        firstface: 0,
        numfaces: 0,
    }];
    b
}

/// [`floor_bsp`] with a 16-unit stair step: in hull 1 (the player's box) the
/// floor is at origin `z = 24` for `x < 84` and `z = 40` beyond, the riser of
/// a step at `x = 100` pushed out by the box's 16-unit half width. Hull 0
/// keeps the flat floor; only the player's box is traced here.
pub(crate) fn step_bsp() -> Bsp {
    use crate::bsp::{CONTENTS_EMPTY, CONTENTS_SOLID, DClipNode, DPlane};
    let mut b = floor_bsp();
    b.planes.push(DPlane { normal: [1.0, 0.0, 0.0], dist: 84.0, ptype: 0 }); // 2: the riser
    b.planes.push(DPlane { normal: [0.0, 0.0, 1.0], dist: 40.0, ptype: 2 }); // 3: the step's top
    // Clipnode 0 splits on the riser: in front (x >= 84) clipnode 1, the
    // step's top; behind it clipnode 2, the floor below.
    b.clipnodes = vec![
        DClipNode { planenum: 2, children: [1, 2] },
        DClipNode { planenum: 3, children: [CONTENTS_EMPTY as i16, CONTENTS_SOLID as i16] },
        DClipNode { planenum: 1, children: [CONTENTS_EMPTY as i16, CONTENTS_SOLID as i16] },
    ];
    b
}

/// Build a progs for the player-physics tests. It declares every field the
/// client movement code reads/writes and the engine globals it sets, plus a
/// `PutClientInServer` function that sets `health = 100` and `origin =
/// (0, 0, 40)` (above the floor) by storing two prepared global constants.
/// `SetNewParms` / `ClientConnect` / `PlayerPreThink` / `PlayerPostThink` /
/// `StartFrame` are empty (just `DONE`) so the connect/frame paths run. The
/// system functions are resolved by NAME (no need to wire the like-named
/// globals — `connect_client`'s `sys_function` falls back to `find_function`).
///
/// Returns `(image, g_const100_ofs, g_origin_vec_ofs)` so the test can place
/// the `100.0` float and the `(0,0,40)` vector the spawn function stores.
pub(crate) fn player_progs() -> (Vec<u8>, usize, usize) {
    player_progs_with_prethink(vec![Statement { op: Op::Done, a: 0, b: 0, c: 0 }])
}

/// [`player_progs`] with `prethink` as the body of `PlayerPreThink` (the global
/// `prethink_time` at offset 56 is free for it to record into).
pub(crate) fn player_progs_with_prethink(prethink: Vec<Statement>) -> (Vec<u8>, usize, usize) {
    let mut b = Builder::new();
    b.add_global("prethink_time", EV_FLOAT, 56);
    b.entityfields = 48;

    // Engine globals the server sets/reads.
    b.add_global("self", EV_ENTITY, 31);
    b.add_global("other", EV_ENTITY, 32);
    b.add_global("time", EV_FLOAT, 33);
    b.add_global("world", EV_ENTITY, 34);
    b.add_global("frametime", EV_FLOAT, 35);
    b.add_global("viewentity", EV_FLOAT, 36);
    b.add_global("v_forward", EV_VECTOR, 60);
    b.add_global("v_right", EV_VECTOR, 63);
    b.add_global("v_up", EV_VECTOR, 66);

    // Constants the spawn function stores: 100.0 (health) and (0,0,40)
    // (origin). Placed in free global cells; the test fills them after load.
    let g_const100 = 40u16;
    let g_origin = 44u16; // 44,45,46

    // Fields the client physics touches.
    b.add_field("classname", EV_STRING, 1);
    b.add_field("origin", EV_VECTOR, 2); // 2,3,4
    b.add_field("velocity", EV_VECTOR, 5); // 5,6,7
    b.add_field("mins", EV_VECTOR, 8); // 8,9,10
    b.add_field("maxs", EV_VECTOR, 11); // 11,12,13
    b.add_field("absmin", EV_VECTOR, 14); // 14,15,16
    b.add_field("absmax", EV_VECTOR, 17); // 17,18,19
    b.add_field("angles", EV_VECTOR, 20); // 20,21,22
    b.add_field("v_angle", EV_VECTOR, 23); // 23,24,25
    b.add_field("punchangle", EV_VECTOR, 26); // 26,27,28
    b.add_field("size", EV_VECTOR, 29); // 29,30,31
    b.add_field("flags", EV_FLOAT, 32);
    b.add_field("health", EV_FLOAT, 33);
    b.add_field("movetype", EV_FLOAT, 34);
    b.add_field("solid", EV_FLOAT, 35);
    b.add_field("fixangle", EV_FLOAT, 36);
    b.add_field("teleport_time", EV_FLOAT, 37);
    b.add_field("groundentity", EV_ENTITY, 38);
    b.add_field("view_ofs", EV_VECTOR, 39); // 39,40,41
    b.add_field("model", EV_STRING, 42);
    b.add_field("modelindex", EV_FLOAT, 43);
    b.add_field("think", EV_FUNCTION, 44);
    b.add_field("nextthink", EV_FLOAT, 45);
    b.add_field("touch", EV_FUNCTION, 46);
    b.add_field("gravity", EV_FLOAT, 47);

    // Field offsets for the spawn function's stores (health=33, origin=2..4).
    let f_health = 33u16;
    let f_origin = 2u16;

    // Empty system functions (DONE only).
    let done = || Statement { op: Op::Done, a: 0, b: 0, c: 0 };
    b.add_function("SetNewParms", vec![done()]);
    b.add_function("ClientConnect", vec![done()]);
    b.add_function("StartFrame", vec![done()]);
    b.add_function("PlayerPreThink", prethink);
    b.add_function("PlayerPostThink", vec![done()]);

    // PutClientInServer: STOREP_F const100 -> self.health;
    //                    STOREP_V origin_const -> self.origin; DONE.
    // We compute the field pointer with ADDRESS(self, field) -> a temp global,
    // then STOREP into it. Use temp globals 48 (ptr) and the self entity in
    // global 31. Field-number globals: we need a global holding the field ofs.
    // Simpler: ADDRESS takes (entity, field) where both are globals; place the
    // field numbers in globals 50 (health) and 51 (origin).
    let g_fhealth = 50u16;
    let g_forigin = 51u16;
    let g_ptr = 52u16;
    let put = b.add_function(
        "PutClientInServer",
        vec![
            // ptr = ADDRESS(self, f_health)
            Statement {
                op: Op::Address,
                a: 31, // self entity global
                b: g_fhealth as i16,
                c: g_ptr as i16,
            },
            // *ptr = const100
            Statement { op: Op::StorepF, a: g_const100 as i16, b: g_ptr as i16, c: 0 },
            // ptr = ADDRESS(self, f_origin)
            Statement { op: Op::Address, a: 31, b: g_forigin as i16, c: g_ptr as i16 },
            // *ptr = origin_const (vector)
            Statement { op: Op::StorepV, a: g_origin as i16, b: g_ptr as i16, c: 0 },
            done(),
        ],
    );
    let _ = put;
    // The ADDRESS field-number globals (g_fhealth=50, g_forigin=51) and the
    // value constants are filled by `prime_player_globals` after the test
    // builds its Server, keeping these offsets in one documented place.
    let _ = (g_fhealth, g_forigin, g_ptr, f_health, f_origin);

    let img = b.build();
    (img, g_const100 as usize, g_origin as usize)
}

/// Set up the field-number constants a freshly-loaded player progs needs for
/// its `PutClientInServer` ADDRESS ops, plus the value constants. Mirrors the
/// offsets chosen in [`player_progs`].
pub(crate) fn prime_player_globals(server: &mut Server, g_const100: usize, g_origin: usize) {
    // Field numbers for ADDRESS (health field ofs 33, origin field ofs 2).
    server.vm.set_gi(50, 33);
    server.vm.set_gi(51, 2);
    // Value constants: health 100, origin (0,0,40).
    server.vm.set_gf(g_const100, 100.0);
    server.vm.set_gv(g_origin, [0.0, 0.0, 40.0]);
}

/// Field/global offsets the attack progs uses (kept in one place so the test
/// can fill the constants after load).
pub(crate) mod attack_ofs {
    // Globals.
    pub const SELF: u16 = 31;
    pub const G_FIRED: u16 = 40; // float flag PostThink sets when attacking
    pub const G_BTN: u16 = 41; // temp: loaded self.button0
    pub const G_FBUTTON0: u16 = 42; // holds the button0 field offset (for LOAD)
    pub const G_ONE: u16 = 43; // const 1.0
    pub const G_SNDFUNC: u16 = 44; // const: function index of the sound builtin
    pub const G_CHAN: u16 = 45; // const channel
    pub const G_VOL: u16 = 46; // const volume
    pub const G_ATTEN: u16 = 47; // const attenuation
    pub const G_SAMPLE: u16 = 48; // const string_t of the sample name
    // Field offsets.
    pub const F_BUTTON0: u16 = 48; // button0 field cell
}

/// Build a progs whose `PlayerPostThink` reads `self.button0` and, when it is
/// set, both sets a global flag (`g_fired = 1`) and fires `sound(self, CHAN,
/// SAMPLE, VOL, ATTEN)` through the engine `PF_sound` builtin (#8). When
/// `button0` is clear it does nothing. Returns `(image, sound_fn_index)`; the
/// caller fills the constant globals via [`prime_attack_globals`].
pub(crate) fn attack_progs() -> (Vec<u8>, usize) {
    use attack_ofs::*;
    let mut b = Builder::new();
    b.entityfields = 56;

    b.add_global("self", EV_ENTITY, SELF);
    b.add_global("other", EV_ENTITY, 32);
    b.add_global("time", EV_FLOAT, 33);
    b.add_global("world", EV_ENTITY, 34);
    b.add_global("frametime", EV_FLOAT, 35);
    b.add_global("viewentity", EV_FLOAT, 36);
    b.add_global("v_forward", EV_VECTOR, 60);
    b.add_global("v_right", EV_VECTOR, 63);
    b.add_global("v_up", EV_VECTOR, 66);
    // A named global for the flag so the test can read it by name.
    b.add_global("fired_flag", EV_FLOAT, G_FIRED);

    // Fields the client physics touches (mirrors player_progs' broad set so
    // the movement path never faults), plus button0/weapon/ammo_shells.
    b.add_field("classname", EV_STRING, 1);
    b.add_field("origin", EV_VECTOR, 2); // 2,3,4
    b.add_field("velocity", EV_VECTOR, 5); // 5,6,7
    b.add_field("mins", EV_VECTOR, 8); // 8,9,10
    b.add_field("maxs", EV_VECTOR, 11); // 11,12,13
    b.add_field("absmin", EV_VECTOR, 14); // 14,15,16
    b.add_field("absmax", EV_VECTOR, 17); // 17,18,19
    b.add_field("angles", EV_VECTOR, 20); // 20,21,22
    b.add_field("v_angle", EV_VECTOR, 23); // 23,24,25
    b.add_field("punchangle", EV_VECTOR, 26); // 26,27,28
    b.add_field("size", EV_VECTOR, 29); // 29,30,31
    b.add_field("flags", EV_FLOAT, 32);
    b.add_field("health", EV_FLOAT, 33);
    b.add_field("movetype", EV_FLOAT, 34);
    b.add_field("solid", EV_FLOAT, 35);
    b.add_field("fixangle", EV_FLOAT, 36);
    b.add_field("teleport_time", EV_FLOAT, 37);
    b.add_field("groundentity", EV_ENTITY, 38);
    b.add_field("view_ofs", EV_VECTOR, 39); // 39,40,41
    b.add_field("think", EV_FUNCTION, 44);
    b.add_field("nextthink", EV_FLOAT, 45);
    b.add_field("touch", EV_FUNCTION, 46);
    b.add_field("gravity", EV_FLOAT, 47);
    b.add_field("button0", EV_FLOAT, F_BUTTON0);
    b.add_field("button2", EV_FLOAT, 49);
    b.add_field("impulse", EV_FLOAT, 50);
    b.add_field("weapon", EV_FLOAT, 51);
    b.add_field("ammo_shells", EV_FLOAT, 52);
    b.add_field("effects", EV_FLOAT, 53);

    // The sound builtin (PF_sound, #8) as a callable QuakeC function.
    let sound_fn = b.add_builtin("sound", 8);

    // Empty connect/frame system functions.
    let done = || Statement { op: Op::Done, a: 0, b: 0, c: 0 };
    b.add_function("SetNewParms", vec![done()]);
    b.add_function("ClientConnect", vec![done()]);
    b.add_function("PutClientInServer", vec![done()]);
    b.add_function("StartFrame", vec![done()]);
    b.add_function("PlayerPreThink", vec![done()]);

    // PlayerPostThink: read self.button0; if set, fire the sound + set flag.
    // Statement layout (relative indices used for the IFNOT branch offset):
    //   0 LoadF  self.button0 -> G_BTN
    //   1 IFNOT  G_BTN -> (skip to DONE at rel index 9)  => offset 8
    //   2 StoreF G_ONE -> fired_flag
    //   3 StoreEnt self -> PARM0
    //   4 StoreF G_CHAN -> PARM1
    //   5 StoreS G_SAMPLE -> PARM2
    //   6 StoreF G_VOL -> PARM3
    //   7 StoreF G_ATTEN -> PARM4
    //   8 CALL5  G_SNDFUNC
    //   9 DONE
    let parm0 = OFS_PARM0 as i16; // 4
    let parm1 = (OFS_PARM0 + 3) as i16; // 7
    let parm2 = (OFS_PARM0 + 6) as i16; // 10
    let parm3 = (OFS_PARM0 + 9) as i16; // 13
    let parm4 = (OFS_PARM0 + 12) as i16; // 16
    b.add_function(
        "PlayerPostThink",
        vec![
            Statement { op: Op::LoadF, a: SELF as i16, b: G_FBUTTON0 as i16, c: G_BTN as i16 },
            Statement { op: Op::Ifnot, a: G_BTN as i16, b: 8, c: 0 },
            Statement { op: Op::StoreF, a: G_ONE as i16, b: G_FIRED as i16, c: 0 },
            Statement { op: Op::StoreEnt, a: SELF as i16, b: parm0, c: 0 },
            Statement { op: Op::StoreF, a: G_CHAN as i16, b: parm1, c: 0 },
            Statement { op: Op::StoreS, a: G_SAMPLE as i16, b: parm2, c: 0 },
            Statement { op: Op::StoreF, a: G_VOL as i16, b: parm3, c: 0 },
            Statement { op: Op::StoreF, a: G_ATTEN as i16, b: parm4, c: 0 },
            Statement { op: Op::Call5, a: G_SNDFUNC as i16, b: 0, c: 0 },
            done(),
        ],
    );

    (b.build(), sound_fn)
}

/// Fill the constant globals the attack progs reads (after the Server is
/// built so the sample string is interned into the live VM heap).
pub(crate) fn prime_attack_globals(server: &mut Server, sound_fn: usize, sample: &str) -> i32 {
    use attack_ofs::*;
    server.vm.set_gi(G_FBUTTON0 as usize, F_BUTTON0 as i32);
    server.vm.set_gf(G_ONE as usize, 1.0);
    server.vm.set_gi(G_SNDFUNC as usize, sound_fn as i32);
    server.vm.set_gf(G_CHAN as usize, 1.0); // CHAN_WEAPON
    server.vm.set_gf(G_VOL as usize, 1.0);
    server.vm.set_gf(G_ATTEN as usize, 1.0); // ATTN_NORM
    let s_t = server.vm.intern(sample);
    server.vm.set_gi(G_SAMPLE as usize, s_t);
    s_t
}

/// Build a minimal progs for the changelevel parm-marshaling tests. It
/// declares the engine globals plus `parm1..parm16`, a `classname`/`origin`
/// field set, and three system functions:
///   * `SetChangeParms`: copies a test-filled constant into `parm1` (the
///     QuakeC `SetChangeParms` marshals the player's state into the parm
///     globals; here we just write a recognisable value so the test can prove
///     `save_spawn_parms` ran it and read it back).
///   * `ClientConnect`: empty (DONE).
///   * `PutClientInServer`: copies `parm1` back into a global `decoded` so a
///     test can prove the restored parm was visible to the spawn script
///     (mirrors `DecodeLevelParms` reading parm1 into a player field).
///
/// Returns `(image, g_const_ofs, g_decoded_ofs)` so the test can place the
/// value `SetChangeParms` stores and read what `PutClientInServer` decoded.
pub(crate) fn changelevel_progs() -> (Vec<u8>, usize, usize) {
    let mut b = Builder::new();
    b.entityfields = 8;

    // Engine globals + the 16 spawn parms. Globals 31..36 are the well-known
    // self/other/time/world/frametime/viewentity (matching player_progs).
    b.add_global("self", EV_ENTITY, 31);
    b.add_global("other", EV_ENTITY, 32);
    b.add_global("time", EV_FLOAT, 33);
    b.add_global("world", EV_ENTITY, 34);
    b.add_global("frametime", EV_FLOAT, 35);
    b.add_global("viewentity", EV_FLOAT, 36);
    // parm1..parm16 at globals 70..85.
    for i in 0..NUM_SPAWN_PARMS {
        b.add_global(&parm_global_name(i), EV_FLOAT, 70 + i as u16);
    }
    // A constant SetChangeParms stores into parm1, and a global
    // PutClientInServer decodes parm1 into. Filled by the test after load.
    let g_const = 40u16;
    let g_decoded = 41u16;

    // Minimal field set so spawn()/link/connect work.
    b.add_field("classname", EV_STRING, 1);
    b.add_field("origin", EV_VECTOR, 2);
    b.add_field("mins", EV_VECTOR, 5);
    b.add_field("maxs", EV_VECTOR, 8);
    b.add_field("absmin", EV_VECTOR, 11);
    b.add_field("absmax", EV_VECTOR, 14);
    b.add_field("flags", EV_FLOAT, 17);
    b.add_field("movetype", EV_FLOAT, 18);
    b.add_field("solid", EV_FLOAT, 19);
    b.add_field("size", EV_VECTOR, 20);
    b.add_field("health", EV_FLOAT, 23);

    let done = || Statement { op: Op::Done, a: 0, b: 0, c: 0 };
    // SetChangeParms: parm1 = g_const.
    b.add_function(
        "SetChangeParms",
        vec![
            Statement {
                op: Op::StoreF,
                a: g_const as i16,
                b: 70, // parm1 global ofs
                c: 0,
            },
            done(),
        ],
    );
    b.add_function("ClientConnect", vec![done()]);
    // PutClientInServer: g_decoded = parm1 (DecodeLevelParms stand-in).
    b.add_function(
        "PutClientInServer",
        vec![
            Statement {
                op: Op::StoreF,
                a: 70, // parm1 global ofs
                b: g_decoded as i16,
                c: 0,
            },
            done(),
        ],
    );

    let img = b.build();
    (img, g_const as usize, g_decoded as usize)
}
