//! The asset commands — `info`, `ls`, `cat`, `bsp`, `map`, `mdl`, `spr`,
//! `wad`, `dis` and `run`: what the `quake_rs` loaders read from id's files
//! (PAK, WAD2, BSP, MDL, SPR and `progs.dat`), printed as text. None of them
//! needs the engine beyond the loaders and, for `run`, the bare QuakeC VM.

use std::fmt::Write as _;

use quake_rs::bsp::{self, Bsp};
use quake_rs::mdl::{Frame as MFrame, Mdl, Skin};
use quake_rs::pak::Pak;
use quake_rs::progs::{OFS_RETURN, Progs};
use quake_rs::spr::{Frame as SFrame, Sprite};
use quake_rs::vm::Vm;
use quake_rs::wad::{self, Wad2};

use crate::entities::count_entity_classnames;
use crate::{CmdResult, Out, read};

/// `info`: sniff the magic and dispatch to the right summary.
pub fn cmd_info(path: &str) -> CmdResult {
    let bytes = read(path)?;
    let mut magic = [0u8; 4];
    let n = bytes.len().min(4);
    magic[..n].copy_from_slice(&bytes[..n]);
    match &magic {
        b"PACK" => cmd_ls(path),
        b"WAD2" => cmd_wad(path),
        b"IDPO" => cmd_mdl(path),
        b"IDSP" => cmd_spr(path),
        _ => {
            // BSP has no string magic — its first int is the version (29).
            if bytes.len() >= 4 && i32::from_le_bytes(magic) == bsp::BSPVERSION {
                cmd_bsp(path)
            } else {
                Err(format!("unrecognized format (first bytes: {:02x?})", &bytes[..n]).into())
            }
        }
    }
}

pub fn cmd_ls(path: &str) -> CmdResult {
    let pak = Pak::open(path)?;
    let mut o = String::new();
    let _ = writeln!(
        o,
        "PAK  {}  ({} files, dir crc {:#06x}{})",
        path,
        pak.entries().len(),
        pak.dir_crc(),
        if pak.is_modified() { ", modified" } else { ", stock pak0" }
    );
    let mut total: i64 = 0;
    for e in pak.entries() {
        total += e.filelen.max(0) as i64;
        let _ = writeln!(o, "  {:>9}  {}", e.filelen, e.name);
    }
    let _ = writeln!(o, "  {} bytes of file data across {} entries", total, pak.entries().len());
    Ok(Out::Text(o))
}

pub fn cmd_cat(path: &str, name: &str) -> CmdResult {
    let pak = Pak::open(path)?;
    let data = pak.read_file(name)?.ok_or_else(|| format!("{name:?} not found in {path}"))?;
    Ok(Out::Bytes(data))
}

pub fn cmd_wad(path: &str) -> CmdResult {
    let bytes = read(path)?;
    let w = Wad2::parse(bytes)?;
    let mut o = String::new();
    let _ = writeln!(o, "WAD2  {}  ({} lumps)", path, w.lumps().len());
    for l in w.lumps() {
        let _ = writeln!(o, "  {:>8}  {:<16}  type {} ({})", l.disksize, l.name, l.typ, typ_name(l.typ));
    }
    Ok(Out::Text(o))
}

fn typ_name(t: u8) -> &'static str {
    match t {
        wad::TYP_NONE => "none",
        wad::TYP_LABEL => "label",
        wad::TYP_PALETTE => "palette",
        wad::TYP_QTEX => "qtex",
        wad::TYP_QPIC => "qpic",
        wad::TYP_SOUND => "sound",
        wad::TYP_MIPTEX => "miptex",
        _ => "?",
    }
}

pub fn cmd_bsp(path: &str) -> CmdResult {
    let bytes = read(path)?;
    let b = Bsp::parse(&bytes)?;
    let mut o = String::new();
    let _ = writeln!(o, "BSP v{}  {}", b.version, path);
    let _ = writeln!(o, "  planes       {}", b.planes.len());
    let _ = writeln!(o, "  vertexes     {}", b.vertexes.len());
    let _ = writeln!(o, "  edges        {}", b.edges.len());
    let _ = writeln!(o, "  faces        {}", b.faces.len());
    let _ = writeln!(o, "  nodes        {}", b.nodes.len());
    let _ = writeln!(o, "  leafs        {}", b.leafs.len());
    let _ = writeln!(o, "  clipnodes    {}", b.clipnodes.len());
    let _ = writeln!(o, "  texinfo      {}", b.texinfo.len());
    let _ = writeln!(o, "  models       {}", b.models.len());
    let _ = writeln!(o, "  marksurfaces {}", b.marksurfaces.len());
    let _ = writeln!(o, "  surfedges    {}", b.surfedges.len());
    let _ = writeln!(
        o,
        "  textures     {} ({} present)",
        b.textures.len(),
        b.textures.iter().filter(|t| t.is_some()).count()
    );
    let _ = writeln!(o, "  visibility   {} bytes", b.visibility.len());
    let _ = writeln!(o, "  lighting     {} bytes", b.lighting.len());
    let _ = writeln!(o, "  entities     {} bytes", b.entities.len());

    if let Some(m) = b.models.first() {
        let _ = writeln!(o, "\n  worldmodel bounds: mins {:?}  maxs {:?}", m.mins, m.maxs);
    }

    let classnames = count_entity_classnames(&b.entities);
    if !classnames.is_empty() {
        let _ = writeln!(o, "\n  top entity classnames:");
        let mut v: Vec<_> = classnames.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        for (name, n) in v.into_iter().take(12) {
            let _ = writeln!(o, "    {:>4}  {}", n, name);
        }
    }

    let textures: Vec<&str> = b.textures.iter().filter_map(|t| t.as_ref().map(|m| m.name.as_str())).take(12).collect();
    if !textures.is_empty() {
        let _ = writeln!(o, "\n  some textures: {}", textures.join(", "));
    }
    Ok(Out::Text(o))
}

pub fn cmd_map(path: &str) -> CmdResult {
    let bytes = read(path)?;
    let b = Bsp::parse(&bytes)?;
    if b.vertexes.is_empty() {
        return Err("BSP has no vertices to plot".into());
    }

    const W: usize = 78;
    const H: usize = 32;

    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for v in &b.vertexes {
        min_x = min_x.min(v.point[0]);
        max_x = max_x.max(v.point[0]);
        min_y = min_y.min(v.point[1]);
        max_y = max_y.max(v.point[1]);
    }
    let span_x = (max_x - min_x).max(1.0);
    let span_y = (max_y - min_y).max(1.0);

    let mut grid = vec![vec![b' '; W]; H];
    for v in &b.vertexes {
        let fx = (v.point[0] - min_x) / span_x;
        let fy = (v.point[1] - min_y) / span_y;
        let gx = ((fx * (W as f32 - 1.0)).round() as i64).clamp(0, W as i64 - 1) as usize;
        // Flip Y so north (+Y) is up on screen.
        let gy = (((1.0 - fy) * (H as f32 - 1.0)).round() as i64).clamp(0, H as i64 - 1) as usize;
        grid[gy][gx] = b'#';
    }

    let mut o = String::new();
    let _ = writeln!(
        o,
        "top-down view of {}  ({} verts, x:[{:.0},{:.0}] y:[{:.0},{:.0}])",
        path,
        b.vertexes.len(),
        min_x,
        max_x,
        min_y,
        max_y
    );
    let _ = writeln!(o, "+{}+", "-".repeat(W));
    for row in &grid {
        // grid holds only ASCII ' '/'#'.
        let _ = writeln!(o, "|{}|", String::from_utf8_lossy(row));
    }
    let _ = writeln!(o, "+{}+", "-".repeat(W));
    Ok(Out::Text(o))
}

pub fn cmd_mdl(path: &str) -> CmdResult {
    let bytes = read(path)?;
    let m = Mdl::parse(&bytes)?;
    let h = &m.header;
    let mut o = String::new();
    let _ = writeln!(o, "MDL  {}  (alias model, version {})", path, h.version);
    let _ = writeln!(o, "  skin size    {} x {}", h.skinwidth, h.skinheight);
    let _ = writeln!(o, "  vertices     {}", h.numverts);
    let _ = writeln!(o, "  triangles    {}", h.numtris);
    let _ = writeln!(o, "  scale        {:?}", h.scale);
    let _ = writeln!(o, "  scale_origin {:?}", h.scale_origin);
    let _ = writeln!(o, "  eyeposition  {:?}", h.eyeposition);
    let _ = writeln!(o, "  bound radius {}", h.boundingradius);
    let _ = writeln!(o, "  flags        {:#x}", h.flags);

    let (mut single_skins, mut group_skins) = (0, 0);
    for s in &m.skins {
        match s {
            Skin::Single(_) => single_skins += 1,
            Skin::Group { .. } => group_skins += 1,
        }
    }
    let _ = writeln!(o, "  skins        {} ({} single, {} group)", m.skins.len(), single_skins, group_skins);

    let _ = writeln!(o, "  frames       {}", m.frames.len());
    for (i, f) in m.frames.iter().enumerate().take(16) {
        match f {
            MFrame::Single(af) => {
                let _ = writeln!(o, "    [{i}] single  {:?}", af.name);
            }
            MFrame::Group { frames, .. } => {
                let _ = writeln!(o, "    [{i}] group   {} poses", frames.len());
            }
        }
    }
    Ok(Out::Text(o))
}

pub fn cmd_spr(path: &str) -> CmdResult {
    let bytes = read(path)?;
    let s = Sprite::parse(&bytes)?;
    let h = &s.header;
    let mut o = String::new();
    let _ = writeln!(o, "SPR  {}  (sprite, version {})", path, h.version);
    let _ = writeln!(o, "  max size     {} x {}", h.width, h.height);
    let _ = writeln!(o, "  orientation  {} ({})", h.type_, spr_orient(h.type_));
    let _ = writeln!(o, "  bound radius {}", h.boundingradius);
    let _ = writeln!(o, "  frames       {}", s.frames.len());
    for (i, f) in s.frames.iter().enumerate().take(16) {
        match f {
            SFrame::Single(sf) => {
                let _ = writeln!(o, "    [{i}] single  {}x{}", sf.width, sf.height);
            }
            SFrame::Group { frames, .. } => {
                let _ = writeln!(o, "    [{i}] group   {} frames", frames.len());
            }
        }
    }
    Ok(Out::Text(o))
}

fn spr_orient(t: i32) -> &'static str {
    match t {
        quake_rs::spr::SPR_VP_PARALLEL_UPRIGHT => "vp-parallel-upright",
        quake_rs::spr::SPR_FACING_UPRIGHT => "facing-upright",
        quake_rs::spr::SPR_VP_PARALLEL => "vp-parallel",
        quake_rs::spr::SPR_ORIENTED => "oriented",
        quake_rs::spr::SPR_VP_PARALLEL_ORIENTED => "vp-parallel-oriented",
        _ => "?",
    }
}

// ---------------------------------------------------------------- QuakeC VM ---

pub fn cmd_dis(path: &str) -> CmdResult {
    let bytes = read(path)?;
    let p = Progs::parse(&bytes)?;
    let mut o = String::new();
    let _ = writeln!(
        o,
        "progs.dat  version {}  ({} functions, {} statements, {} globals, {}B strings, {} entityfields)\n",
        p.version,
        p.functions.len(),
        p.statements.len(),
        p.globals.len(),
        p.strings.len(),
        p.entityfields
    );
    o.push_str(&p.disassemble());
    Ok(Out::Text(o))
}

pub fn cmd_run(path: &str, func: &str) -> CmdResult {
    let bytes = read(path)?;
    let mut vm = Vm::load(&bytes)?;
    vm.call_by_name(func)?;
    let mut o = String::new();
    if !vm.output().is_empty() {
        let _ = writeln!(o, "--- output ---");
        o.push_str(vm.output().trim_end());
        o.push('\n');
    }
    let _ = writeln!(o, "--- {func}() returned: float={} int={} ---", vm.gf(OFS_RETURN), vm.gi(OFS_RETURN));
    Ok(Out::Text(o))
}
