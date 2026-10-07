//! `map gen:NAME`: small maps made for the film, where id's own maps cannot
//! show a point cleanly (a long wall seen at a grazing angle, for the
//! perspective span). The game loads one like any other map: the film puts
//! it in front of the search path as `maps/gen_NAME.bsp` ([`layer`]).
//!
//! There is no map compiler here (id's `qbsp`, `light` and `vis`), so this
//! module writes a version-29 BSP itself, for the one shape that needs none
//! of a compiler's cleverness: a box room, seen from inside. Every lump is
//! what `Mod_LoadBrushModel` (model.c) reads, laid out as id's tools lay it:
//!
//! - **The tree.** A box room is convex, so its BSP is a chain: a node for
//!   each of its six walls, the room in front of it or behind, solid on the
//!   other side. The room is leaf 1; leaf 0 is the solid outside every map
//!   has. Each node holds the faces on its plane, as qbsp's do. Planes are
//!   axial with a positive normal (types 0 to 2), as qbsp writes them, since
//!   the engine's axial fast paths (`BOX_ON_PLANE_SIDE`, the hull traces)
//!   take the normal to be +1 on the plane's axis.
//! - **The faces** are cut on one grid ([`Room::tile`]), so a wall meets the
//!   floor along the same vertices (no T-junction to crack) and no face is
//!   more than 256 texels across (`CalcSurfaceExtents` refuses more; qbsp
//!   cut at 240). Each is wound as qbsp winds it, clockwise seen from the
//!   side it faces, and neighbours share their edges as qbsp's `GetEdge`
//!   shares them, so the edge renderer meets the same edges it meets in
//!   id's maps.
//! - **The textures** are id's, copied out of the shareware maps, and
//!   mapped as qbsp's `TextureAxisFromPlane` maps a brush face at id's
//!   scale (a texel a unit): a wall's S along the wall, its T down.
//! - **The light** is id's `light` program's formula (`SingleLightFace`: a
//!   light's level less its distance, half of it weighted by the cosine,
//!   `rangescale` 0.5), one sample every 16 texels from `texturemins`, as
//!   `R_BuildLightMap` reads them. There are no shadows to cast: nothing in
//!   a convex room stands between a light and a wall.
//! - **No vis.** Every leaf's `visofs` is -1, so each sees all the others
//!   (`mod_novis`), as a map before `vis` ran.
//! - **The clip hulls** (1, the player's box; 2, the Shambler's) are the
//!   room shrunk by each box: a clip node a wall.
//!
//! The entities are the game's own: an `info_player_start`, and `light`
//! points that only the light pass reads (QuakeC removes them, as it does
//! from id's maps).

use std::collections::HashMap;

use quake_rs::bsp::{self, Bsp, MipTex};
use quake_rs::pak::{self, Pak};

/// A shot's `map gen:NAME` names a generated map.
pub const PREFIX: &str = "gen:";

/// The maps this module makes.
pub const NAMES: &[&str] = &["grazing"];

/// The name the game knows a generated map by: `maps/gen_grazing.bsp` for
/// `gen:grazing`.
pub fn map_name(name: &str) -> String {
    format!("gen_{name}")
}

/// The search path `pak` with the generated map `name` in front of it.
pub fn layer(pak: Pak, name: &str) -> Result<Pak, String> {
    let bytes = generate(name, &pak)?;
    let file = format!("maps/{}.bsp", map_name(name));
    let top =
        Pak::from_bytes(format!("{PREFIX}{name}"), pak::write_pack(&[(&file, &bytes)])).map_err(|e| e.to_string())?;
    Ok(top.over(pak))
}

/// `quaketool mapgen <pak> <name> <out> [--pak1]`: write the generated map
/// `name` to `out`, as a `.bsp`, or as a `.pak` holding `maps/gen_NAME.bsp`
/// for `quaketool view`'s pak list. `--pak1` adds id's `gfx/pop.lmp` to the
/// pack, so id's own engine (the oracle's) takes it as the registered
/// `pak1.pak`: the shareware engine reads no map but pak0's otherwise
/// (`COM_FindFile`, `COM_CheckRegistered`).
pub fn cmd_mapgen(args: &[String]) -> Result<String, String> {
    let (pak_path, name, out) = (&args[0], &args[1], &args[2]);
    let pak1 = match args.get(3).map(String::as_str) {
        None => false,
        Some("--pak1") => true,
        Some(other) => return Err(format!("mapgen: unknown argument {other:?}")),
    };
    let pak = Pak::open(pak_path).map_err(|e| format!("{pak_path}: {e}"))?;
    let bytes = generate(name, &pak)?;
    let file = format!("maps/{}.bsp", map_name(name));
    let data = if out.ends_with(".pak") {
        let pop = quake_rs::common::pop_lmp();
        let mut files: Vec<(&str, &[u8])> = vec![(&file, &bytes)];
        if pak1 {
            files.insert(0, ("gfx/pop.lmp", &pop));
        }
        pak::write_pack(&files)
    } else if pak1 {
        return Err("mapgen: --pak1 writes a .pak".into());
    } else {
        bytes.clone()
    };
    std::fs::write(out, &data).map_err(|e| format!("cannot write {out}: {e}"))?;
    Ok(format!("mapgen: {PREFIX}{name}, {} bytes ({file}) -> {out}\n", bytes.len()))
}

/// The BSP file of the generated map `name`, its textures copied from the
/// maps in `pak`.
pub fn generate(name: &str, pak: &Pak) -> Result<Vec<u8>, String> {
    let room = match name {
        "grazing" => grazing(),
        _ => return Err(format!("no generated map {name:?} (there are: {})", NAMES.join(", "))),
    };
    room.build(pak)
}

/// `gen:grazing`: a long hall of id's base (e1m1's and the start map's
/// textures), 4096 units long: its walls `tech08_1`, whose slats and bronze
/// bands make long straight lines, lit by a row of lights under the
/// ceiling. Looking down the hall, from its middle or along a wall, the
/// walls run off to their vanishing point at a grazing angle, where a
/// perspective span bends a texel most.
fn grazing() -> Room {
    let (len, wide, high) = (4096, 320, 256);
    let mut ents = vec![Ent::new("info_player_start", [96.0, 160.0, 24.0]).angle(0.0)];
    // A light every 256 units down the middle, under the ceiling.
    for k in 0..len / 256 {
        let x = (128 + 256 * k) as f32;
        ents.push(Ent::new("light", [x, wide as f32 / 2.0, high as f32 - 48.0]).light(450.0));
    }
    Room {
        size: [len, wide, high],
        tile: 128,
        sides: ["tech09_3", "tech09_3", "tech08_1", "tech08_1", "sfloor4_2", "tech10_1"],
        message: "A grazing wall",
        ents,
    }
}

/// An entity of the map: its class, place and what the light pass reads.
#[derive(Debug, Clone)]
struct Ent {
    class: &'static str,
    origin: [f32; 3],
    angle: Option<f32>,
    light: Option<f32>,
}

impl Ent {
    fn new(class: &'static str, origin: [f32; 3]) -> Ent {
        Ent { class, origin, angle: None, light: None }
    }

    fn angle(mut self, a: f32) -> Ent {
        self.angle = Some(a);
        self
    }

    fn light(mut self, l: f32) -> Ent {
        self.light = Some(l);
        self
    }

    /// `light`'s reading of it: a class beginning `light` is a light, its
    /// level the `light` key's or `DEFAULTLIGHTLEVEL`, 300.
    fn light_level(&self) -> Option<f32> {
        self.class.starts_with("light").then(|| self.light.unwrap_or(300.0))
    }
}

/// A box room: `[0, size]` on each axis, seen from inside.
#[derive(Debug, Clone)]
struct Room {
    size: [i32; 3],
    /// The grid every face is cut on, in world units (at most 256: a texel a
    /// unit, `CalcSurfaceExtents`' limit).
    tile: i32,
    /// The walls' textures, in [`Side`] order: x = 0, x = max, y = 0, y =
    /// max, the floor (z = 0) and the ceiling.
    sides: [&'static str; 6],
    message: &'static str,
    ents: Vec<Ent>,
}

/// Wall `k` of a box room: on axis `k / 2`, at its low end (the room in
/// front of the plane) or its high end (the room behind).
#[derive(Debug, Clone, Copy)]
struct Side(usize);

impl Side {
    fn axis(self) -> usize {
        self.0 / 2
    }

    fn high(self) -> bool {
        self.0 % 2 == 1
    }

    /// The direction the wall faces, into the room.
    fn facing(self) -> [f32; 3] {
        let mut n = [0.0; 3];
        n[self.axis()] = if self.high() { -1.0 } else { 1.0 };
        n
    }

    /// qbsp's `TextureAxisFromPlane` for the face: floor and ceiling S along
    /// x, T along -y; a wall facing x S along y, one facing y S along x, T
    /// down.
    fn texture_axes(self) -> ([f32; 3], [f32; 3]) {
        match self.axis() {
            0 => ([0.0, 1.0, 0.0], [0.0, 0.0, -1.0]),
            1 => ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
            _ => ([1.0, 0.0, 0.0], [0.0, -1.0, 0.0]),
        }
    }
}

/// The boxes of the clip hulls 1 and 2 (`Mod_LoadClipnodes`' `clip_mins`
/// and `clip_maxs`): a wall at the room's low end moves in by `-mins`, one
/// at its high end by `maxs`.
const HULLS: [([f32; 3], [f32; 3]); 2] =
    [([-16.0, -16.0, -24.0], [16.0, 16.0, 32.0]), ([-32.0, -32.0, -24.0], [32.0, 32.0, 64.0])];

/// A face being written: its plane (a wall's), the side it faces, its
/// texinfo, its edges and its light.
struct Face {
    plane: usize,
    side: i16,
    firstedge: usize,
    numedges: usize,
    texinfo: usize,
    lightofs: usize,
}

/// The lumps as they are built.
#[derive(Default)]
struct Lumps {
    planes: Vec<([f32; 3], f32, i32)>,
    verts: Vec<[f32; 3]>,
    vert_index: HashMap<[i32; 3], u16>,
    /// Edge 0 is never used (a face's edge `-0` would be `0`).
    edges: Vec<[u16; 2]>,
    /// Edges used once, by their first face, waiting for a neighbour to use
    /// them backwards (qbsp's `GetEdge`).
    open: HashMap<(u16, u16), usize>,
    surfedges: Vec<i32>,
    faces: Vec<Face>,
    lighting: Vec<u8>,
}

impl Lumps {
    fn vert(&mut self, p: [i32; 3]) -> Result<u16, String> {
        if let Some(&i) = self.vert_index.get(&p) {
            return Ok(i);
        }
        let i = u16::try_from(self.verts.len()).map_err(|_| "too many vertices".to_string())?;
        self.verts.push(p.map(|c| c as f32));
        self.vert_index.insert(p, i);
        Ok(i)
    }

    /// The edge from `a` to `b` as a face's surfedge: an edge a neighbour
    /// used from `b` to `a`, backwards (negative), or a new one.
    fn edge(&mut self, a: u16, b: u16) -> Result<i32, String> {
        if let Some(i) = self.open.remove(&(b, a)) {
            return Ok(-(i as i32));
        }
        if self.edges.is_empty() {
            self.edges.push([0, 0]);
        }
        let i = self.edges.len();
        if i > i32::MAX as usize {
            return Err("too many edges".into());
        }
        self.edges.push([a, b]);
        self.open.insert((a, b), i);
        Ok(i as i32)
    }
}

/// A texture's coordinates of a point (`CalcSurfaceExtents`' `val`).
fn tex_coord(v: [f32; 4], p: [f32; 3]) -> f32 {
    p[0] * v[0] + p[1] * v[1] + p[2] * v[2] + v[3]
}

impl Room {
    fn build(&self, pak: &Pak) -> Result<Vec<u8>, String> {
        let mut l = Lumps::default();
        // The planes: hull 0's six walls, then hull 1's and hull 2's.
        let insets: [([f32; 3], [f32; 3]); 3] = [([0.0; 3], [0.0; 3]), HULLS[0], HULLS[1]];
        for (mins, maxs) in insets {
            for k in 0..6 {
                let s = Side(k);
                let a = s.axis();
                let dist = if s.high() { self.size[a] as f32 - maxs[a] } else { -mins[a] };
                let mut normal = [0.0; 3];
                normal[a] = 1.0;
                l.planes.push((normal, dist, a as i32));
            }
        }

        // The textures, each once, and a texinfo a wall.
        let mut names: Vec<&str> = Vec::new();
        for s in self.sides {
            if !names.contains(&s) {
                names.push(s);
            }
        }
        let textures = names.iter().map(|n| find_texture(pak, n)).collect::<Result<Vec<_>, _>>()?;
        let texinfo: Vec<([[f32; 4]; 2], usize)> = (0..6)
            .map(|k| {
                let (s, t) = Side(k).texture_axes();
                let v = |a: [f32; 3]| [a[0], a[1], a[2], 0.0];
                let miptex = names.iter().position(|n| *n == self.sides[k]).unwrap_or(0);
                ([v(s), v(t)], miptex)
            })
            .collect();

        // The lights, as `light` reads the entities.
        let lights: Vec<([f32; 3], f32)> =
            self.ents.iter().filter_map(|e| e.light_level().map(|v| (e.origin, v))).collect();

        // The faces, a wall at a time (so each node's are consecutive).
        let mut node_faces = Vec::new();
        for (k, &(vecs, _)) in texinfo.iter().enumerate() {
            let side = Side(k);
            let a = side.axis();
            let (u, v) = ((a + 1) % 3, (a + 2) % 3);
            let first = l.faces.len();
            let at = if side.high() { self.size[a] } else { 0 };
            for v0 in (0..self.size[v]).step_by(self.tile as usize) {
                for u0 in (0..self.size[u]).step_by(self.tile as usize) {
                    let (u1, v1) = ((u0 + self.tile).min(self.size[u]), (v0 + self.tile).min(self.size[v]));
                    let point = |pu: i32, pv: i32| {
                        let mut p = [0; 3];
                        p[a] = at;
                        p[u] = pu;
                        p[v] = pv;
                        p
                    };
                    // Anticlockwise about +axis; reversed where the wall faces +axis,
                    // so each face is clockwise seen from the side it faces.
                    let mut corners = [point(u0, v0), point(u1, v0), point(u1, v1), point(u0, v1)];
                    if !side.high() {
                        corners.reverse();
                    }
                    let ids = corners.iter().map(|&p| l.vert(p)).collect::<Result<Vec<_>, _>>()?;
                    let firstedge = l.surfedges.len();
                    for i in 0..4 {
                        let e = l.edge(ids[i], ids[(i + 1) % 4])?;
                        l.surfedges.push(e);
                    }
                    let lightofs = l.lighting.len();
                    let samples = light_face(side, vecs, &corners, &lights)?;
                    l.lighting.extend_from_slice(&samples);
                    // `side` 0: the face looks along its plane's normal (a low
                    // wall's, the room in front); 1: against it.
                    l.faces.push(Face {
                        plane: k,
                        side: i16::from(side.high()),
                        firstedge,
                        numedges: 4,
                        texinfo: k,
                        lightofs,
                    });
                }
            }
            node_faces.push((first, l.faces.len() - first));
        }
        Ok(self.write(&l, &texinfo, &textures, &node_faces))
    }

    /// The entity lump: worldspawn, then each entity.
    fn entities(&self) -> String {
        let num = |x: f32| if x.fract() == 0.0 { format!("{}", x as i64) } else { format!("{x}") };
        let mut s =
            format!("{{\n\"classname\" \"worldspawn\"\n\"message\" \"{}\"\n\"worldtype\" \"0\"\n}}\n", self.message);
        for e in &self.ents {
            s += &format!(
                "{{\n\"classname\" \"{}\"\n\"origin\" \"{} {} {}\"\n",
                e.class,
                num(e.origin[0]),
                num(e.origin[1]),
                num(e.origin[2])
            );
            if let Some(a) = e.angle {
                s += &format!("\"angle\" \"{}\"\n", num(a));
            }
            if let Some(v) = e.light {
                s += &format!("\"light\" \"{}\"\n", num(v));
            }
            s += "}\n";
        }
        s
    }

    /// The file: the header, then the lumps in the order qbsp wrote them.
    fn write(
        &self,
        l: &Lumps,
        texinfo: &[([[f32; 4]; 2], usize)],
        textures: &[MipTex],
        node_faces: &[(usize, usize)],
    ) -> Vec<u8> {
        let mut lumps: [Vec<u8>; bsp::HEADER_LUMPS] = Default::default();
        let size = self.size.map(|c| c as f32);
        let bounds = |o: &mut Vec<u8>| {
            for c in [0, 0, 0] {
                o.extend_from_slice(&(c as i16).to_le_bytes());
            }
            for c in self.size {
                o.extend_from_slice(&(c as i16).to_le_bytes());
            }
        };

        let o = &mut lumps[bsp::LUMP_PLANES];
        for (normal, dist, ty) in &l.planes {
            for c in normal {
                o.extend_from_slice(&c.to_le_bytes());
            }
            o.extend_from_slice(&dist.to_le_bytes());
            o.extend_from_slice(&ty.to_le_bytes());
        }

        // Leaf 0, the solid outside; leaf 1, the room, every face its own.
        let o = &mut lumps[bsp::LUMP_LEAFS];
        o.extend_from_slice(&bsp::CONTENTS_SOLID.to_le_bytes());
        o.extend_from_slice(&(-1i32).to_le_bytes());
        o.extend_from_slice(&[0; 20]);
        o.extend_from_slice(&bsp::CONTENTS_EMPTY.to_le_bytes());
        o.extend_from_slice(&(-1i32).to_le_bytes());
        bounds(o);
        o.extend_from_slice(&0u16.to_le_bytes());
        o.extend_from_slice(&(l.faces.len() as u16).to_le_bytes());
        o.extend_from_slice(&[0; 4]);

        let o = &mut lumps[bsp::LUMP_VERTEXES];
        for p in &l.verts {
            for c in p {
                o.extend_from_slice(&c.to_le_bytes());
            }
        }

        // The chain: node k's front is the room (or the next wall's node)
        // for a low wall, solid for a high one.
        let o = &mut lumps[bsp::LUMP_NODES];
        for (k, &(first, count)) in node_faces.iter().enumerate() {
            let next: i16 = if k == 5 { -2 } else { k as i16 + 1 };
            let children = if Side(k).high() { [-1, next] } else { [next, -1] };
            o.extend_from_slice(&(k as i32).to_le_bytes());
            for c in children {
                o.extend_from_slice(&c.to_le_bytes());
            }
            bounds(o);
            o.extend_from_slice(&(first as u16).to_le_bytes());
            o.extend_from_slice(&(count as u16).to_le_bytes());
        }

        let o = &mut lumps[bsp::LUMP_TEXINFO];
        for (vecs, miptex) in texinfo {
            for v in vecs {
                for c in v {
                    o.extend_from_slice(&c.to_le_bytes());
                }
            }
            o.extend_from_slice(&(*miptex as i32).to_le_bytes());
            o.extend_from_slice(&0i32.to_le_bytes());
        }

        let o = &mut lumps[bsp::LUMP_FACES];
        for f in &l.faces {
            o.extend_from_slice(&(f.plane as i16).to_le_bytes());
            o.extend_from_slice(&f.side.to_le_bytes());
            o.extend_from_slice(&(f.firstedge as i32).to_le_bytes());
            o.extend_from_slice(&(f.numedges as i16).to_le_bytes());
            o.extend_from_slice(&(f.texinfo as i16).to_le_bytes());
            o.extend_from_slice(&[0, 255, 255, 255]);
            o.extend_from_slice(&(f.lightofs as i32).to_le_bytes());
        }

        // Hull 1's six clip nodes, then hull 2's, each a chain like hull 0's.
        let o = &mut lumps[bsp::LUMP_CLIPNODES];
        for hull in 0..2 {
            for k in 0..6 {
                let next = if k == 5 { bsp::CONTENTS_EMPTY as i16 } else { (hull * 6 + k + 1) as i16 };
                let solid = bsp::CONTENTS_SOLID as i16;
                let children = if Side(k).high() { [solid, next] } else { [next, solid] };
                o.extend_from_slice(&((6 + hull * 6 + k) as i32).to_le_bytes());
                for c in children {
                    o.extend_from_slice(&c.to_le_bytes());
                }
            }
        }

        let o = &mut lumps[bsp::LUMP_MARKSURFACES];
        for i in 0..l.faces.len() {
            o.extend_from_slice(&(i as u16).to_le_bytes());
        }
        let o = &mut lumps[bsp::LUMP_SURFEDGES];
        for e in &l.surfedges {
            o.extend_from_slice(&e.to_le_bytes());
        }
        let o = &mut lumps[bsp::LUMP_EDGES];
        for e in &l.edges {
            o.extend_from_slice(&e[0].to_le_bytes());
            o.extend_from_slice(&e[1].to_le_bytes());
        }

        // The world, model 0: its bounds, its hulls' head nodes (hull 0's
        // node 0, hull 1's clip node 0, hull 2's clip node 6), one leaf.
        let o = &mut lumps[bsp::LUMP_MODELS];
        for c in [0.0f32; 3].iter().chain(&size).chain(&[0.0f32; 3]) {
            o.extend_from_slice(&c.to_le_bytes());
        }
        for head in [0i32, 0, 6, 0] {
            o.extend_from_slice(&head.to_le_bytes());
        }
        o.extend_from_slice(&1i32.to_le_bytes());
        o.extend_from_slice(&0i32.to_le_bytes());
        o.extend_from_slice(&(l.faces.len() as i32).to_le_bytes());

        lumps[bsp::LUMP_LIGHTING] = l.lighting.clone();
        lumps[bsp::LUMP_ENTITIES] = self.entities().into_bytes();
        lumps[bsp::LUMP_ENTITIES].push(0);

        // The textures: their count, each one's offset, then each miptex
        // (`miptex_t` and its four levels).
        let o = &mut lumps[bsp::LUMP_TEXTURES];
        o.extend_from_slice(&(textures.len() as i32).to_le_bytes());
        let mut at = 4 + 4 * textures.len();
        let mut body = Vec::new();
        for t in textures {
            o.extend_from_slice(&(at as i32).to_le_bytes());
            let mut name = [0u8; 16];
            let n = t.name.len().min(15);
            name[..n].copy_from_slice(&t.name.as_bytes()[..n]);
            body.extend_from_slice(&name);
            body.extend_from_slice(&t.width.to_le_bytes());
            body.extend_from_slice(&t.height.to_le_bytes());
            let mut ofs = bsp::MIPTEX_SIZE;
            let levels: Vec<&[u8]> = (0..bsp::MIPLEVELS).filter_map(|m| t.mip(m)).collect();
            for level in &levels {
                body.extend_from_slice(&(ofs as u32).to_le_bytes());
                ofs += level.len();
            }
            for level in &levels {
                body.extend_from_slice(level);
            }
            at += ofs;
        }
        o.extend_from_slice(&body);

        // The header and the lumps, each at a 4-byte boundary.
        const ORDER: [usize; 15] = [
            bsp::LUMP_PLANES,
            bsp::LUMP_LEAFS,
            bsp::LUMP_VERTEXES,
            bsp::LUMP_NODES,
            bsp::LUMP_TEXINFO,
            bsp::LUMP_FACES,
            bsp::LUMP_CLIPNODES,
            bsp::LUMP_MARKSURFACES,
            bsp::LUMP_SURFEDGES,
            bsp::LUMP_EDGES,
            bsp::LUMP_MODELS,
            bsp::LUMP_LIGHTING,
            bsp::LUMP_VISIBILITY,
            bsp::LUMP_ENTITIES,
            bsp::LUMP_TEXTURES,
        ];
        let mut out = vec![0u8; bsp::HEADER_SIZE];
        out[..4].copy_from_slice(&bsp::BSPVERSION.to_le_bytes());
        for k in ORDER {
            let ofs = out.len();
            out.extend_from_slice(&lumps[k]);
            out[4 + 8 * k..8 + 8 * k].copy_from_slice(&(ofs as i32).to_le_bytes());
            out[8 + 8 * k..12 + 8 * k].copy_from_slice(&(lumps[k].len() as i32).to_le_bytes());
            while out.len() % 4 != 0 {
                out.push(0);
            }
        }
        out
    }
}

/// A face's light samples: `light`'s `SingleLightFace` for each light at
/// each sample, the samples every 16 texels from `texturemins` over the
/// face's extents (`CalcSurfaceExtents`), one unit in front of the face as
/// `light`'s `texorg` is; then `FinishLightface`'s `rangescale` (0.5) and
/// clamp to 255.
fn light_face(
    side: Side,
    vecs: [[f32; 4]; 2],
    corners: &[[i32; 3]],
    lights: &[([f32; 3], f32)],
) -> Result<Vec<u8>, String> {
    let mut mins = [f32::MAX; 2];
    let mut maxs = [f32::MIN; 2];
    for c in corners {
        let p = c.map(|x| x as f32);
        for j in 0..2 {
            let val = tex_coord(vecs[j], p);
            mins[j] = mins[j].min(val);
            maxs[j] = maxs[j].max(val);
        }
    }
    let texmins = [0, 1].map(|j| (mins[j] / 16.0).floor() as i32 * 16);
    let extents = [0, 1].map(|j| ((maxs[j] / 16.0).ceil() as i32 - (mins[j] / 16.0).floor() as i32) * 16);
    if extents.iter().any(|&e| e > 256) {
        return Err(format!("a face {extents:?} texels across: CalcSurfaceExtents allows 256"));
    }
    let (smax, tmax) = (extents[0] / 16 + 1, extents[1] / 16 + 1);
    let normal = side.facing();
    let a = side.axis();
    let plane = corners[0][a] as f32;
    // A light behind the face's plane, or further from it than its level,
    // lights none of it.
    let reach = |o: [f32; 3]| (o[a] - plane) * normal[a];
    let lights: Vec<_> = lights.iter().filter(|&&(o, level)| reach(o) > 0.0 && reach(o) <= level).collect();
    let mut out = Vec::with_capacity((smax * tmax) as usize);
    for t in 0..tmax {
        for s in 0..smax {
            let (us, ut) = ((texmins[0] + 16 * s) as f32, (texmins[1] + 16 * t) as f32);
            let p = texture_to_world(vecs, a, plane, us, ut, normal);
            let mut total = 0.0f32;
            for &&(origin, level) in &lights {
                let to = [origin[0] - p[0], origin[1] - p[1], origin[2] - p[2]];
                let dist = (to[0] * to[0] + to[1] * to[1] + to[2] * to[2]).sqrt();
                // `scalecos` 0.5: half the light is weighted by the angle.
                let angle = 0.5 + 0.5 * (to[0] * normal[0] + to[1] * normal[1] + to[2] * normal[2]) / dist;
                let add = (level - dist) * angle;
                if add > 0.0 {
                    total += add;
                }
            }
            // `rangescale` 0.5, then clamped.
            out.push((total * 0.5).min(255.0) as u8);
        }
    }
    Ok(out)
}

/// The point of the face's plane (axis `a` at `plane`) whose texture
/// coordinates are `(s, t)`, one unit in front of it.
fn texture_to_world(vecs: [[f32; 4]; 2], a: usize, plane: f32, s: f32, t: f32, normal: [f32; 3]) -> [f32; 3] {
    let mut p = [0.0; 3];
    p[a] = plane + normal[a];
    // Each of an axial face's texture axes lies along one of its other axes.
    for (v, val) in [(vecs[0], s), (vecs[1], t)] {
        if let Some(k) = (0..3).find(|&k| k != a && v[k] != 0.0) {
            p[k] = (val - v[3]) / v[k];
        }
    }
    p
}

/// id's texture `name`, from the first of the shareware maps that has it.
fn find_texture(pak: &Pak, name: &str) -> Result<MipTex, String> {
    const MAPS: [&str; 9] = ["start", "e1m1", "e1m2", "e1m3", "e1m4", "e1m5", "e1m6", "e1m7", "e1m8"];
    for m in MAPS {
        let Ok(Some(bytes)) = pak.read_file(&format!("maps/{m}.bsp")) else { continue };
        let Ok(b) = Bsp::parse(&bytes) else { continue };
        if let Some(t) = b.textures.into_iter().flatten().find(|t| t.name == name && t.mip(3).is_some()) {
            return Ok(t);
        }
    }
    Err(format!("texture {name:?} is in none of the shareware maps"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use quake_rs::render::{self, Camera, PerspSpan, Renderer, Scene};
    use quake_rs::world::{build_hull, hull_point_contents};

    fn pak() -> Option<Pak> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../quake-data/ID1/PAK0.PAK");
        p.exists().then(|| Pak::open(p).expect("the pak opens"))
    }

    fn fnv(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
    }

    fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }

    /// A face's corners in its surfedges' order (`CalcSurfaceExtents`' walk).
    fn corners(b: &Bsp, f: &bsp::DFace) -> Vec<[f32; 3]> {
        (0..f.numedges as usize)
            .map(|i| {
                let e = b.surfedges[f.firstedge as usize + i];
                let v = if e >= 0 { b.edges[e as usize].v[0] } else { b.edges[(-e) as usize].v[1] };
                b.vertexes[v as usize].point
            })
            .collect()
    }

    #[test]
    fn the_hall_is_a_sealed_box_as_mod_load_brush_model_reads_one() {
        let Some(pak) = pak() else { return };
        let bytes = generate("grazing", &pak).expect("generates");
        // The generator's output is a fixture of the film's: the same bytes every time.
        assert_eq!(bytes, generate("grazing", &pak).unwrap());
        assert_eq!(fnv(&bytes), 0x4ebb_aa2f_90f4_afdd, "the hall's bytes");
        let b = Bsp::parse(&bytes).expect("parses");
        assert_eq!((b.leafs.len(), b.nodes.len(), b.clipnodes.len(), b.models.len()), (2, 6, 12, 1));
        assert_eq!(b.leafs[0].contents, bsp::CONTENTS_SOLID);
        assert_eq!(b.leafs[1].nummarksurfaces as usize, b.faces.len());
        // No vis: the room's leaf sees everything (`mod_novis`).
        assert!(b.leaf_pvs(1).iter().skip(1).all(|&v| v));
        let mut uses: HashMap<i32, (u32, u32)> = HashMap::new();
        for f in &b.faces {
            // Wound clockwise seen from the side the face is seen from.
            let p = corners(&b, f);
            let n = b.planes[f.planenum as usize].normal;
            let c = sub(p[1], p[0]);
            let d = sub(p[2], p[1]);
            let w = [c[1] * d[2] - c[2] * d[1], c[2] * d[0] - c[0] * d[2], c[0] * d[1] - c[1] * d[0]];
            let along = w[0] * n[0] + w[1] * n[1] + w[2] * n[2];
            assert!(if f.side == 0 { along < 0.0 } else { along > 0.0 }, "face {f:?} wound the wrong way");
            // At most 256 texels across, its light samples inside the lump.
            let ti = &b.texinfo[f.texinfo as usize];
            let ext = [0, 1].map(|j| {
                let vals: Vec<f32> = p.iter().map(|&q| tex_coord(ti.vecs[j], q)).collect();
                let lo = (vals.iter().copied().fold(f32::MAX, f32::min) / 16.0).floor();
                let hi = (vals.iter().copied().fold(f32::MIN, f32::max) / 16.0).ceil();
                ((hi - lo) * 16.0) as usize
            });
            assert!(ext[0] <= 256 && ext[1] <= 256, "{ext:?}");
            assert!(f.lightofs as usize + (ext[0] / 16 + 1) * (ext[1] / 16 + 1) <= b.lighting.len());
            for i in 0..f.numedges as usize {
                let e = b.surfedges[f.firstedge as usize + i];
                let u = uses.entry(e.abs()).or_default();
                if e > 0 { u.0 += 1 } else { u.1 += 1 }
            }
        }
        // Sealed: every edge is two faces', once each way (no T-junctions, no crack).
        assert_eq!(uses.len(), b.edges.len() - 1);
        assert!(uses.values().all(|&u| u == (1, 1)), "an edge not shared by exactly two faces");
        // The hulls: the room is empty, its walls solid, the player's box 16
        // units from a wall and 24 above the floor, the Shambler's 32.
        let contents = |hull: usize, p: [f32; 3]| {
            let h = build_hull(&b, hull);
            hull_point_contents(&h, b.models[0].headnode[hull], p)
        };
        let (e, s) = (bsp::CONTENTS_EMPTY, bsp::CONTENTS_SOLID);
        assert_eq!([contents(0, [200.0, 1.0, 64.0]), contents(0, [200.0, -1.0, 64.0])], [e, s]);
        assert_eq!([contents(1, [200.0, 17.0, 64.0]), contents(1, [200.0, 15.0, 64.0])], [e, s]);
        assert_eq!([contents(1, [200.0, 160.0, 25.0]), contents(1, [200.0, 160.0, 23.0])], [e, s]);
        assert_eq!([contents(2, [200.0, 33.0, 64.0]), contents(2, [200.0, 31.0, 64.0])], [e, s]);
        assert_eq!([contents(2, [4096.0 - 33.0, 160.0, 64.0]), contents(2, [200.0, 160.0, 256.0 - 63.0])], [e, s]);
    }

    #[test]
    fn every_pixel_of_the_hall_is_a_wall() {
        // The background (`r_clearcolor`, index 2) shows wherever no face is
        // drawn: a crack between faces, or a face wound the wrong way (its
        // spans come out inverted and draw nothing). Take index 2 out of the
        // textures and the colormap, and no pixel may be 2, from inside the
        // hall, at id's span and exact, looking down it, across it and into
        // its corners.
        let Some(pak) = pak() else { return };
        let mut b = Bsp::parse(&generate("grazing", &pak).unwrap()).unwrap();
        for t in b.textures.iter_mut().flatten() {
            for level in std::iter::once(&mut t.pixels).chain(t.mips.iter_mut()) {
                level.iter_mut().filter(|p| **p == 2).for_each(|p| *p = 1);
            }
        }
        let palette = render::parse_palette(&pak.read_file("gfx/palette.lmp").unwrap().unwrap()).unwrap();
        let mut colormap = pak.read_file("gfx/colormap.lmp").unwrap().unwrap();
        colormap.iter_mut().filter(|p| **p == 2).for_each(|p| *p = 1);
        let views = [
            ([200.0, 16.0, 56.0], [4000.0, 16.0, 56.0]),
            ([3000.0, 160.0, 128.0], [3000.0, 0.0, 100.0]),
            ([3900.0, 200.0, 40.0], [4096.0, 320.0, 256.0]),
            ([100.0, 300.0, 200.0], [0.0, 0.0, 0.0]),
        ];
        for (from, to) in views {
            for span in [PerspSpan::Spans16, PerspSpan::Exact] {
                let mut scene = Scene::new(&b, Camera::looking_at(from, to, 90.0), 320, 200, &palette);
                scene.colormap = Some(&colormap);
                scene.options.persp_span = span;
                let img = Renderer::new().render(&scene);
                let gaps = img.pixels.iter().filter(|&&p| p == 2).count();
                assert_eq!(gaps, 0, "{gaps} background pixels from {from:?} to {to:?} at {span:?}");
            }
        }
    }

    #[test]
    fn the_hall_loads_as_a_map_and_films_the_same_frames_every_time() {
        let Some(pak) = super::super::v2_tests::pak_path() else { return };
        let shot = "map gen:grazing\nduration 0.15\nfps 20\nsize 320x180\nwarmup 0.2\ncamera path\n\
                    key 0 160,16,56 0,0 ease linear\nkey 0.15 176,16,56 0,0\n";
        let a = super::super::tests::hashes(&pak, shot, 1, "gen");
        assert_eq!(a.len(), 3);
        assert_eq!(a, super::super::tests::hashes(&pak, shot, 4, "gen"), "the same frames on 1 and 4 threads");
        assert!(a.windows(2).all(|w| w[0] != w[1]), "the camera moves");
    }
}
