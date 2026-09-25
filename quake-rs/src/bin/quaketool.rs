//! `quaketool` — inspect Quake assets using the `quake_rs` loaders.
//!
//! ```text
//! quaketool info <file>          auto-detect format and print a summary
//! quaketool ls   <pak>           list a PAK archive's directory
//! quaketool cat  <pak> <name>    write one file from a PAK to stdout
//! quaketool bsp  <file.bsp>      dump BSP lump counts, model bounds, entity keys
//! quaketool map  <file.bsp>      render a top-down ASCII minimap from BSP geometry
//! quaketool mdl  <file.mdl>      dump an alias model's header, skins, frames
//! quaketool spr  <file.spr>      dump a sprite's header and frames
//! quaketool wad  <file.wad>      list a WAD2 archive's lumps
//! ```
//!
//! Output is buffered and written once, so piping into `head`/`less` (which
//! closes the pipe early) exits cleanly instead of panicking on `BrokenPipe`.

use std::fmt::Write as _;
use std::io::Write as _;
use std::process::exit;

use quake_rs::bsp::{self, Bsp};
use quake_rs::dlight::DynamicLights;
use quake_rs::mdl::{Frame as MFrame, Mdl, Skin};
use quake_rs::pak::Pak;
use quake_rs::particles::{Lcg, ParticleSystem};
use quake_rs::progs::{Progs, OFS_RETURN};
use quake_rs::render::{self, Camera};
use quake_rs::server::{Server, TempEntityEvent, UserCmd};
use quake_rs::spr::{Frame as SFrame, Sprite};
use quake_rs::vm::Vm;
use quake_rs::wad::{self, Wad2};

/// What a command produced: text to print, or raw bytes (for `cat`).
enum Out {
    Text(String),
    Bytes(Vec<u8>),
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        usage();
        exit(2);
    }
    let cmd = args[1].as_str();
    let rest = &args[2..];

    let result = match cmd {
        "info" => need(rest, 1, cmd).and_then(|a| cmd_info(&a[0])),
        "ls" => need(rest, 1, cmd).and_then(|a| cmd_ls(&a[0])),
        "cat" => need(rest, 2, cmd).and_then(|a| cmd_cat(&a[0], &a[1])),
        "bsp" => need(rest, 1, cmd).and_then(|a| cmd_bsp(&a[0])),
        "map" => need(rest, 1, cmd).and_then(|a| cmd_map(&a[0])),
        "mdl" => need(rest, 1, cmd).and_then(|a| cmd_mdl(&a[0])),
        "spr" => need(rest, 1, cmd).and_then(|a| cmd_spr(&a[0])),
        "wad" => need(rest, 1, cmd).and_then(|a| cmd_wad(&a[0])),
        "dis" => need(rest, 1, cmd).and_then(|a| cmd_dis(&a[0])),
        "run" => need(rest, 2, cmd).and_then(|a| cmd_run(&a[0], &a[1])),
        "render" => need(rest, 2, cmd).and_then(|a| cmd_render(&a[0], &a[1], a.get(2).map(|s| s.as_str()))),
        "render-demo" => need(rest, 1, cmd).and_then(|a| cmd_render_demo(&a[0])),
        "menu" => need(rest, 2, cmd).and_then(|a| cmd_menu(&a[0], &a[1])),
        "scene" => need(rest, 3, cmd).and_then(|a| cmd_scene(&a[0], &a[1], &a[2])),
        "view" => need(rest, 3, cmd).and_then(cmd_view),
        "walk" => need(rest, 3, cmd).and_then(|a| {
            cmd_walk(&a[0], &a[1], &a[2], a.get(3).and_then(|s| s.parse().ok()).unwrap_or(40))
        }),
        "demo" => need(rest, 3, cmd).and_then(|a| {
            cmd_demo(&a[0], &a[1], &a[2], a.get(3).and_then(|s| s.parse().ok()).unwrap_or(0))
        }),
        "playtest" => need(rest, 2, cmd).and_then(|a| cmd_playtest(&a[0], &a[1], a.get(2).map(|s| s.as_str()))),
        "simbench" => need(rest, 2, cmd).and_then(|a| {
            cmd_simbench(&a[0], &a[1], a.get(2).and_then(|s| s.parse().ok()).unwrap_or(600))
        }),
        "changelevel" => need(rest, 2, cmd).and_then(|a| cmd_changelevel(&a[0], &a[1])),
        "sim" => need(rest, 2, cmd).and_then(|a| {
            cmd_sim(&a[0], &a[1], a.get(2).and_then(|s| s.parse().ok()).unwrap_or(5))
        }),
        "-h" | "--help" | "help" => {
            usage();
            return;
        }
        other => Err(format!("unknown command {other:?}")),
    };

    match result {
        Ok(out) => emit(out),
        Err(e) => {
            eprintln!("quaketool: {e}");
            exit(1);
        }
    }
}

/// Write the output once. A closed downstream pipe (`head`, `less`) is a clean
/// exit, not an error.
fn emit(out: Out) {
    let bytes = match &out {
        Out::Text(s) => s.as_bytes(),
        Out::Bytes(b) => b.as_slice(),
    };
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    match lock.write_all(bytes).and_then(|_| lock.flush()) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => exit(0),
        Err(e) => {
            eprintln!("quaketool: write error: {e}");
            exit(1);
        }
    }
}

fn usage() {
    eprint!(
        "quaketool — inspect Quake assets\n\n\
         USAGE:\n\
         \tquaketool info <file>          auto-detect format and summarize\n\
         \tquaketool ls   <pak>           list a PAK directory\n\
         \tquaketool cat  <pak> <name>    extract one PAK file to stdout\n\
         \tquaketool bsp  <file.bsp>      dump BSP lump counts / models / entities\n\
         \tquaketool map  <file.bsp>      top-down ASCII minimap from BSP geometry\n\
         \tquaketool mdl  <file.mdl>      dump alias-model header/skins/frames\n\
         \tquaketool spr  <file.spr>      dump sprite header/frames\n\
         \tquaketool wad  <file.wad>      list WAD2 lumps\n\
         \tquaketool dis  <progs.dat>     disassemble QuakeC bytecode\n\
         \tquaketool run  <progs.dat> <fn>  execute a QuakeC function, show output + return\n\
         \tquaketool render <bsp> <out.ppm>  software-render a BSP to a PPM image\n\
         \tquaketool render-demo <out.ppm>   render the built-in demo room\n\
         \tquaketool menu <pak> <out.ppm>    draw the MAIN menu over the e1m1 POV\n\
         \tquaketool sim <progs.dat> <bsp> [frames]  spawn a map's QuakeC entities + tick physics\n\
         \tquaketool scene <pak> <map.bsp> <out.ppm>  render a map + its spawned MDL entities\n\
         \tquaketool view <pak> <map.bsp> <out.ppm> [--res WxH] [--origin x,y,z] [--angles p,y,r] [--time T] [--fov F] [--ents FILE] [--viewmodel M:F] [--bench N]\n\
         \t                               render one exact view (Quake camera convention), for the C oracle diff\n\
         \tquaketool walk <pak> <map.bsp> <out-prefix> [steps]  walk forward from spawn; one PPM frame per step\n\
         \tquaketool demo <pak> <demo.dem> <out-prefix> [stride]  replay + render a recorded demo\n\
         \tquaketool playtest <pak> <map.bsp> [out.ppm]  spawn a player, walk forward, report state + render POV\n\
         \tquaketool simbench <pak> <map.bsp> [frames]  benchmark the game-logic tick (physics/VM/AI/collision), no rendering\n\
         \tquaketool changelevel <pak> <map.bsp>  drive a player into the map's exit, swap to the next level, prove inventory carries\n"
    );
}

fn need<'a>(rest: &'a [String], n: usize, cmd: &str) -> Result<&'a [String], String> {
    if rest.len() < n {
        Err(format!("`{cmd}` needs {n} argument(s)"))
    } else {
        Ok(rest)
    }
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))
}

/// `info`: sniff the magic and dispatch to the right summary.
fn cmd_info(path: &str) -> Result<Out, String> {
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
                Err(format!("unrecognized format (first bytes: {:02x?})", &bytes[..n]))
            }
        }
    }
}

fn cmd_ls(path: &str) -> Result<Out, String> {
    let pak = Pak::open(path).map_err(|e| e.to_string())?;
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

fn cmd_cat(path: &str, name: &str) -> Result<Out, String> {
    let pak = Pak::open(path).map_err(|e| e.to_string())?;
    let data = pak
        .read_file(name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("{name:?} not found in {path}"))?;
    Ok(Out::Bytes(data))
}

fn cmd_wad(path: &str) -> Result<Out, String> {
    let bytes = read(path)?;
    let w = Wad2::parse(bytes).map_err(|e| e.to_string())?;
    let mut o = String::new();
    let _ = writeln!(o, "WAD2  {}  ({} lumps)", path, w.lumps().len());
    for l in w.lumps() {
        let _ = writeln!(
            o,
            "  {:>8}  {:<16}  type {} ({})",
            l.disksize,
            l.name,
            l.typ,
            typ_name(l.typ)
        );
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

fn cmd_bsp(path: &str) -> Result<Out, String> {
    let bytes = read(path)?;
    let b = Bsp::parse(&bytes).map_err(|e| e.to_string())?;
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

    let textures: Vec<&str> = b
        .textures
        .iter()
        .filter_map(|t| t.as_ref().map(|m| m.name.as_str()))
        .take(12)
        .collect();
    if !textures.is_empty() {
        let _ = writeln!(o, "\n  some textures: {}", textures.join(", "));
    }
    Ok(Out::Text(o))
}

/// Tally `"classname" "x"` values out of the raw entity lump text. A deliberately
/// tiny scanner (not the full QuakeC entity parser) — enough to summarize a map.
fn count_entity_classnames(ents: &str) -> std::collections::HashMap<String, u32> {
    let mut out = std::collections::HashMap::new();
    let mut prev_key: Option<String> = None;
    for (idx, tok) in ents.split('"').enumerate() {
        // Quoted strings sit at odd token indices.
        if idx % 2 == 1 {
            match prev_key.take() {
                Some(k) if k == "classname" => {
                    *out.entry(tok.to_string()).or_insert(0) += 1;
                }
                Some(_) => {}
                None => prev_key = Some(tok.to_string()),
            }
        }
    }
    out
}

fn cmd_map(path: &str) -> Result<Out, String> {
    let bytes = read(path)?;
    let b = Bsp::parse(&bytes).map_err(|e| e.to_string())?;
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
        path, b.vertexes.len(), min_x, max_x, min_y, max_y
    );
    let _ = writeln!(o, "+{}+", "-".repeat(W));
    for row in &grid {
        // grid holds only ASCII ' '/'#'.
        let _ = writeln!(o, "|{}|", String::from_utf8_lossy(row));
    }
    let _ = writeln!(o, "+{}+", "-".repeat(W));
    Ok(Out::Text(o))
}

fn cmd_mdl(path: &str) -> Result<Out, String> {
    let bytes = read(path)?;
    let m = Mdl::parse(&bytes).map_err(|e| e.to_string())?;
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
    let _ = writeln!(
        o,
        "  skins        {} ({} single, {} group)",
        m.skins.len(), single_skins, group_skins
    );

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

fn cmd_spr(path: &str) -> Result<Out, String> {
    let bytes = read(path)?;
    let s = Sprite::parse(&bytes).map_err(|e| e.to_string())?;
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

// ----------------------------------------------------------- server / sim ---

fn contents_name(c: i32) -> &'static str {
    match c {
        -1 => "empty",
        -2 => "solid",
        -3 => "water",
        -4 => "slime",
        -5 => "lava",
        -6 => "sky",
        _ => "?",
    }
}

fn cmd_sim(progs_path: &str, bsp_path: &str, frames: u32) -> Result<Out, String> {
    let pbytes = read(progs_path)?;
    let progs = quake_rs::progs::Progs::parse(&pbytes).map_err(|e| e.to_string())?;
    let bbytes = read(bsp_path)?;
    let bsp = Bsp::parse(&bbytes).map_err(|e| e.to_string())?;

    let mut o = String::new();
    let _ = writeln!(
        o,
        "map {bsp_path}: {} faces, {} models; progs {progs_path}: {} functions, {} entityfields",
        bsp.faces.len(),
        bsp.models.len(),
        progs.functions.len(),
        progs.entityfields
    );

    // --- collision demo: trace straight down from the player spawn ---
    if let Some((spawn, _ang)) = player_start(&bsp.entities) {
        let from = [spawn[0], spawn[1], spawn[2] + 24.0];
        let to = [from[0], from[1], from[2] - 4096.0];
        let (mins, maxs) = ([-16.0, -16.0, -24.0], [16.0, 16.0, 32.0]);
        let tr = quake_rs::world::trace_world(&bsp, from, to, mins, maxs);
        let _ = writeln!(o, "\ncollision trace (player box, straight down from spawn):");
        let _ = writeln!(o, "  start {from:?}  contents={}", contents_name(quake_rs::world::point_contents(&bsp, from)));
        if tr.fraction < 1.0 {
            let _ = writeln!(
                o,
                "  hit floor at {:?}  ({:.1} units below)  normal {:?}",
                tr.endpos,
                from[2] - tr.endpos[2],
                tr.plane_normal
            );
        } else {
            let _ = writeln!(o, "  no hit within 4096 units (fraction {:.3})", tr.fraction);
        }
    }

    // --- spawn the map's QuakeC entities ---
    let mut server = Server::new(bsp, progs).map_err(|e| e.to_string())?;
    // SV_SpawnServer set sv.modelname/world.model/mapname before loading
    // entities; derive the bare name from the bsp file stem.
    if let Some(stem) = std::path::Path::new(bsp_path).file_stem().and_then(|s| s.to_str()) {
        server.set_map_name(stem);
    }
    let rep = server.spawn_entities().map_err(|e| e.to_string())?;
    let _ = writeln!(
        o,
        "\nspawn: {} entity blocks -> {} spawned, {} inhibited (skill), {} no-spawn-fn, {} spawn errors",
        rep.total, rep.spawned, rep.inhibited, rep.no_spawn_function, rep.spawn_errors
    );
    let _ = writeln!(o, "  live edicts: {}", server.live_entities());
    let _ = writeln!(o, "  top classnames spawned:");
    for (name, n) in rep.classnames.iter().take(15) {
        let _ = writeln!(o, "    {n:>4}  {name}");
    }

    // --- tick physics ---
    if frames > 0 {
        let mut total = 0usize;
        let mut errs = 0usize;
        for _ in 0..frames {
            let fr = server.run_frame(0.1).map_err(|e| e.to_string())?;
            total += fr.thinks_fired;
            errs += fr.think_errors;
        }
        let _ = writeln!(
            o,
            "\nphysics: {frames} frames @ dt=0.1 -> time {:.1}s, {total} think calls fired, {errs} think errors (unimplemented builtins)",
            server.time()
        );
    }
    Ok(Out::Text(o))
}

// ------------------------------------------------------------- playtest -----

/// The explosion sound a rocket/grenade/tarbaby temp entity plays (the C
/// `cl_sfx_r_exp3` = `weapons/r_exp3.wav`).
const TE_EXPLOSION_SOUND: &str = "weapons/r_exp3.wav";

/// Realise one decoded [`TempEntityEvent`] into `particles`, porting the
/// effect-mapping half of `CL_ParseTEnt`:
///
/// * explosion types (`TE_EXPLOSION`=3, `TE_TAREXPLOSION`=4, `TE_EXPLOSION2`=12)
///   spawn a 1024-particle [`ParticleSystem::spawn_explosion`] and return the
///   `weapons/r_exp3.wav` sound name to play;
/// * impact types spawn a `R_RunParticleEffect`-style burst (the existing
///   [`ParticleSystem::spawn_burst`]) with the matching colour/count:
///   `TE_SPIKE`=0 -> (0,10), `TE_SUPERSPIKE`=1 / `TE_GUNSHOT`=2 -> (0,20),
///   `TE_WIZSPIKE`=7 -> (20,30), `TE_KNIGHTSPIKE`=8 -> (226,20);
/// * splashes (`TE_LAVASPLASH`=10, `TE_TELEPORT`=11) get a small upward burst;
/// * beams (`TE_LIGHTNING1/2/3`=5/6/9, `TE_BEAM`=13) are skipped (consumed only).
///
/// Returns `Some(sound_name)` for the types that play a sound, else `None`.
fn spawn_temp_entity(
    particles: &mut ParticleSystem,
    ev: &TempEntityEvent,
    now: f32,
    rng: &mut Lcg,
) -> Option<&'static str> {
    use quake_rs::server::te_consts::*;
    match ev.te_type {
        TE_EXPLOSION | TE_TAREXPLOSION | TE_EXPLOSION2 => {
            particles.spawn_explosion(ev.pos, now, rng);
            Some(TE_EXPLOSION_SOUND)
        }
        TE_SPIKE => {
            particles.spawn_burst(ev.pos, [0.0; 3], 0, 10, now, rng);
            None
        }
        TE_SUPERSPIKE | TE_GUNSHOT => {
            particles.spawn_burst(ev.pos, [0.0; 3], 0, 20, now, rng);
            None
        }
        TE_WIZSPIKE => {
            particles.spawn_burst(ev.pos, [0.0; 3], 20, 30, now, rng);
            None
        }
        TE_KNIGHTSPIKE => {
            particles.spawn_burst(ev.pos, [0.0; 3], 226, 20, now, rng);
            None
        }
        TE_LAVASPLASH | TE_TELEPORT => {
            // Approximate the splash as a small upward burst (dir = +Z).
            particles.spawn_burst(ev.pos, [0.0, 0.0, 1.0], 232, 20, now, rng);
            None
        }
        // Beam/lightning types carry no effect here.
        _ => None,
    }
}

/// Spawn a real player on a map, report its loadout, walk it forward, and render
/// its point of view — the first-person gameplay milestone (#3).
fn cmd_playtest(pak_path: &str, map_name: &str, out: Option<&str>) -> Result<Out, String> {
    let pak = Pak::open(pak_path).map_err(|e| e.to_string())?;
    let read = |n: &str| -> Result<Vec<u8>, String> {
        pak.read_file(n).map_err(|e| e.to_string())?.ok_or_else(|| format!("{n} not found"))
    };
    let bsp_render = Bsp::parse(&read(map_name)?).map_err(|e| e.to_string())?;
    let bsp_sim = Bsp::parse(&read(map_name)?).map_err(|e| e.to_string())?;
    let palette = render::parse_palette(&read("gfx/palette.lmp")?).ok_or("bad palette")?;
    let progs = Progs::parse(&read("progs.dat")?).map_err(|e| e.to_string())?;

    let mut server = Server::with_pak(bsp_sim, progs, Some(pak.clone())).map_err(|e| e.to_string())?;
    server.set_map_name(map_name); // SV_SpawnServer: world.model + the mapname global
    let rep = server.spawn_entities().map_err(|e| e.to_string())?;
    let player = server.connect_client().map_err(|e| format!("connect_client: {e}"))?;

    // Live engine particles (the `particle()` builtin's effect) for this
    // playtest. Bursts fired by the QuakeC during the frame loops below are
    // drained, spawned into this pool with a deterministic RNG + the current
    // game time, aged under gravity, and finally drawn into the POV render so an
    // explosion/gun-impact shows up as coloured points occluded by walls.
    let mut particles = ParticleSystem::new();
    let mut prng = Lcg::new(0x1234_5678);
    // sv_gravity (800) * the R_DrawParticles particle factor (0.05) as an
    // acceleration; ParticleSystem::advance multiplies by dt itself.
    const PARTICLE_GRAVITY: f32 = 800.0 * 0.05;

    let mut o = String::new();
    let _ = writeln!(o, "playtest {map_name}: {} entities spawned; player = edict {player}", rep.spawned);
    let _ = writeln!(
        o,
        "  loadout after PutClientInServer:  health={}  items={:#x}  weapon={}  shells={}",
        server.player_health(),
        server.vm.ent_get_float(player, "items") as i64,
        server.vm.ent_get_float(player, "weapon") as i64,
        server.vm.ent_get_float(player, "ammo_shells") as i64
    );
    let (eye0, ang) = server.player_view();
    let _ = writeln!(o, "  spawn eye {eye0:?}  angles {ang:?}");

    // Walk forward along the spawn yaw for a few seconds of game time. The spawn
    // angle lives on info_player_start in the map entities, so read it from the
    // BSP rather than the (zero) player angle.
    let spawn_yaw = player_start(&bsp_render.entities).map(|(_, a)| a).unwrap_or(ang[1]);
    let cmd = UserCmd { forwardmove: 400.0, yaw: spawn_yaw, ..Default::default() };
    let start = server.vm.ent_get_vector(player, "origin");
    let mut thinks = 0usize;
    for _ in 0..40 {
        let fr = server.client_frame(&cmd, 0.1).map_err(|e| format!("client_frame: {e}"))?;
        thinks += fr.thinks_fired;
    }
    let end = server.vm.ent_get_vector(player, "origin");
    let dx = end[0] - start[0];
    let dy = end[1] - start[1];
    let _ = writeln!(
        o,
        "  walked 40 frames (4.0s): moved {:.0} units, eye now {:?}, health {}",
        (dx * dx + dy * dy).sqrt(),
        server.player_view().0,
        server.player_health()
    );
    let _ = writeln!(o, "  {thinks} entity/monster thinks fired during play");

    // --- doors: did any func_door (MOVETYPE_PUSH) physically move? ---
    {
        // Record every door's origin, then trigger each by calling its `use`
        // function (the QuakeC door_use that buttons/triggers invoke), tick a
        // couple seconds, and report which ones moved. This proves the pusher
        // physics carry the bmodel, independent of whether the player reached
        // the door's specific trigger field.
        let mut doors: Vec<(i32, [f32; 3])> = Vec::new();
        for e in 0..server.vm.num_edicts() {
            let ent = e as i32;
            if server.vm.edict_free.get(e).copied().unwrap_or(true) {
                continue;
            }
            // func_door's spawn reassigns classname to "door"; movetype PUSH (7).
            if server.vm.ent_get_string(ent, "classname") == "door" {
                doors.push((ent, server.vm.ent_get_vector(ent, "origin")));
            }
        }
        let _ = writeln!(o, "\n  {} doors (func_door, classname \"door\", MOVETYPE_PUSH)", doors.len());
        // Fire each door's `use` (self=door, other=player) to open it.
        for &(d, _) in &doors {
            let usefn = server.vm.ent_get_int(d, "use");
            if usefn > 0 {
                server.vm.gset_int("self", d);
                server.vm.gset_int("other", player);
                let _ = server.vm.execute(usefn as usize);
            }
        }
        // Tick ~2s so the doors slide and reach their open state.
        let still = UserCmd { yaw: spawn_yaw, ..Default::default() };
        for _ in 0..20 {
            let _ = server.client_frame(&still, 0.1);
        }
        let mut moved = 0;
        let mut max_disp = 0.0f32;
        for &(d, o0) in &doors {
            if server.vm.edict_free.get(d as usize).copied().unwrap_or(true) {
                continue;
            }
            let o1 = server.vm.ent_get_vector(d, "origin");
            let disp = ((o1[0]-o0[0]).powi(2) + (o1[1]-o0[1]).powi(2) + (o1[2]-o0[2]).powi(2)).sqrt();
            if disp > 1.0 {
                moved += 1;
                max_disp = max_disp.max(disp);
            }
        }
        let _ = writeln!(o, "  after `use` + 2s tick: {moved} doors moved (max displacement {max_disp:.0} units)");
        for s in server.drain_sounds() {
            if s.sample.contains("door") {
                let _ = writeln!(o, "    door sound: {}", s.sample);
                break;
            }
        }
    }

    // --- combat: aim at the nearest monster and pull the trigger ---
    let pe = server.player_view().0;
    let mut nearest: Option<(i32, f32, [f32; 3])> = None;
    for e in 0..server.vm.num_edicts() {
        let ent = e as i32;
        if server.vm.edict_free.get(e).copied().unwrap_or(true) {
            continue;
        }
        if !server.vm.ent_get_string(ent, "classname").starts_with("monster") {
            continue;
        }
        let mo = server.vm.ent_get_vector(ent, "origin");
        let d2 = (mo[0] - pe[0]).powi(2) + (mo[1] - pe[1]).powi(2) + (mo[2] - pe[2]).powi(2);
        if nearest.map_or(true, |(_, bd, _)| d2 < bd) {
            nearest = Some((ent, d2, mo));
        }
    }
    // Snapshot of the particle pool at the frame where the most particles are
    // alive during combat — used for the POV action shot below (declared out
    // here so it outlives the combat block).
    let mut peak_parts: Vec<([f32; 3], u8)> = Vec::new();
    // Dynamic lights (explosions, muzzle flashes, EF_* lights). Driven each
    // combat frame from the drained temp entities + entity_dlights, decayed, and
    // snapshotted at peak so the POV render below lights up the walls.
    let mut dlights = DynamicLights::new();
    let mut peak_dlights: Vec<quake_rs::dlight::DynamicLight> = Vec::new();
    let mut max_active_dlights = 0usize;
    if let Some((mon, d2, mo)) = nearest {
        let mname = server.vm.ent_get_string(mon, "classname");
        let hp_before = server.vm.ent_get_float(mon, "health");
        // Teleport the player ~80 units in front of the monster with clear line
        // of sight, so the shot demonstrably connects (the spawn-walk leaves the
        // player far down the hall behind geometry). Point-blank, same height.
        let approach = [mo[0] - 80.0, mo[1], mo[2] + 24.0];
        server.vm.ent_set_vector(player, "origin", approach);
        let pe = server.player_view().0;
        // Aim the player at the monster (yaw + pitch toward its centre).
        let dir = [mo[0] - pe[0], mo[1] - pe[1], mo[2] - pe[2]];
        let yaw = dir[1].atan2(dir[0]).to_degrees();
        let horiz = (dir[0] * dir[0] + dir[1] * dir[1]).sqrt();
        let pitch = -dir[2].atan2(horiz).to_degrees(); // QuakeC pitch is +down
        let _ = writeln!(
            o,
            "\n  nearest monster: {mname} (edict {mon}) at {:.0} units, health {hp_before}",
            d2.sqrt()
        );
        // Diagnostic: trace a bullet from the eye toward the monster centre and
        // report what the engine's own collision says it hits (the monster, the
        // world, or nothing) — this distinguishes an aim/LOS miss from a damage bug.
        {
            let aim = [pe[0] + dir[0] * 4.0, pe[1] + dir[1] * 4.0, pe[2] + dir[2] * 4.0];
            let tr = quake_rs::server::sv_move(&mut server.vm, pe, aim, [0.0; 3], [0.0; 3], player, false, false);
            let hit = if tr.ent == mon { format!("the monster (edict {mon}) ✓") }
                      else if tr.ent == 0 { "the world (wall) — no LOS".into() }
                      else if tr.ent < 0 { "nothing (clear)".into() }
                      else { format!("another edict {}", tr.ent) };
            let _ = writeln!(o, "  eye {pe:?} -> bullet trace hits {hit} at fraction {:.2}", tr.fraction);
        }
        // AI PROBE: stand still in front of the monster (no firing) and watch
        // whether it acquires the player as its enemy and changes think frames —
        // i.e. whether FindTarget + ai_stand->ai_run transitions fire.
        {
            let still = UserCmd { yaw, pitch, ..Default::default() };
            let _ = writeln!(o, "  AI probe (10 frames, player standing in view):");
            for f in 0..10 {
                server.client_frame(&still, 0.1).map_err(|e| format!("probe: {e}"))?;
                if !server.vm.edict_free.get(mon as usize).copied().unwrap_or(true) {
                    let enemy = server.vm.ent_get_int(mon, "enemy");
                    let frame = server.vm.ent_get_float(mon, "frame");
                    let nextthink = server.vm.ent_get_float(mon, "nextthink");
                    let estate = server.vm.ent_get_float(mon, "enemy"); // raw
                    let _ = estate;
                    if f == 0 || f == 4 || f == 9 {
                        let _ = writeln!(o, "    f{f}: enemy={enemy} frame={frame} nextthink={nextthink:.2}");
                    }
                }
            }
        }
        // Hold attack (buttons bit 0) for ~1.5s of game time; collect sounds.
        let fire = UserCmd { yaw, pitch, buttons: 1, ..Default::default() };
        let mut sounds: Vec<String> = Vec::new();
        let mut total_bursts = 0usize;
        let mut total_burst_particles = 0i64;
        // Temp-entity tallies (rocket/grenade explosions, gunshots, spikes) the
        // QuakeC fires via the Write* network builtins, decoded into events.
        let mut te_total = 0usize;
        let mut te_explosions = 0usize;
        let mut te_gunshots = 0usize;
        for _ in 0..15 {
            server.client_frame(&fire, 0.1).map_err(|e| format!("fire frame: {e}"))?;
            for s in server.drain_sounds() {
                sounds.push(s.sample);
            }
            // Realise any particle() bursts the QuakeC fired (gun impacts, blood,
            // gibs), age the pool, and retire the expired ones — exactly the
            // per-frame cycle the renderer front-end runs.
            let now = server.time();
            for b in server.drain_particles() {
                total_bursts += 1;
                total_burst_particles += b.count.max(0) as i64;
                particles.spawn_burst(b.org, b.dir, b.color, b.count, now, &mut prng);
            }
            // Realise the temp entities (explosions, wall impacts) the QuakeC
            // fired via the Write* builtins. Explosions also queue their sound
            // AND spawn a decaying dynamic light (CL_ParseTEnt: radius 350, die
            // now+0.5, decay 300, minlight 0, key 0 -> a fresh slot each one).
            for ev in server.drain_temp_entities() {
                te_total += 1;
                use quake_rs::server::te_consts::*;
                match ev.te_type {
                    TE_EXPLOSION | TE_TAREXPLOSION | TE_EXPLOSION2 => {
                        te_explosions += 1;
                        dlights.alloc(0, ev.pos, 350.0, now + 0.5, 300.0, 0.0, now);
                    }
                    TE_GUNSHOT => te_gunshots += 1,
                    _ => {}
                }
                if let Some(snd) = spawn_temp_entity(&mut particles, &ev, now, &mut prng) {
                    sounds.push(snd.to_string());
                }
            }
            // Entity light effects (EF_MUZZLEFLASH / BRIGHTLIGHT / DIMLIGHT) from
            // the in-use edicts; add the deterministic rand()&31 radius jitter
            // here (entity_dlights keeps the base radius so the query is pure).
            for ed in server.entity_dlights() {
                let jitter = (prng.next_range(32)) as f32;
                dlights.alloc(
                    ed.key,
                    ed.origin,
                    ed.radius_base + jitter,
                    now + ed.life,
                    0.0,
                    ed.minlight,
                    now,
                );
            }
            particles.advance(0.1, now, PARTICLE_GRAVITY);
            // Decay + retire dynamic lights, then track the peak set for the POV.
            dlights.advance(0.1, now);
            let active = dlights.active();
            if active.len() > max_active_dlights {
                max_active_dlights = active.len();
            }
            if !active.is_empty() && active.len() >= peak_dlights.len() {
                peak_dlights = active;
            }
            if particles.len() > peak_parts.len() {
                peak_parts =
                    particles.particles().iter().map(|p| (p.origin, p.color)).collect();
            }
        }
        let _ = writeln!(
            o,
            "    particle() bursts during combat: {total_bursts} ({total_burst_particles} points); live at end: {}",
            particles.len()
        );
        let _ = writeln!(
            o,
            "    temp entities: {te_total} ({te_explosions} explosions, {te_gunshots} gunshots)"
        );
        let _ = writeln!(
            o,
            "    dynamic lights: peak {max_active_dlights} active during combat (explosions + EF_* muzzle/bright/dim lights)"
        );
        let hp_after = server.vm.ent_get_float(mon, "health");
        let alive = !server.vm.edict_free.get(mon as usize).copied().unwrap_or(true);
        let (b0, weapon, shells) = server.player_attack_state();
        let _ = writeln!(
            o,
            "  fired 15 frames (attack={b0}, weapon={weapon}, shells {} -> {}):",
            25, shells as i64
        );
        let _ = writeln!(
            o,
            "    monster health {hp_before} -> {hp_after}{}",
            if !alive { " (REMOVED — killed)" } else { "" }
        );
        if sounds.is_empty() {
            let _ = writeln!(o, "    (no sound events)");
        } else {
            let _ = writeln!(o, "    sound events: {}", sounds.join(", "));
        }
    } else {
        let _ = writeln!(o, "\n  (no monster in range to attack)");
    }

    // Render the player's POV (world + spawned models).
    if let Some(path) = out {
        let mut model_cache: std::collections::HashMap<String, Option<Mdl>> = std::collections::HashMap::new();
        let mut owned: Vec<(Mdl, [f32; 3], f32, [u8; 3])> = Vec::new();
        let mut bmodels: Vec<render::BModelInstance> = Vec::new();
        // External brush-model item boxes (maps/b_*.bsp): each item's box bsp is
        // parsed once (cached by name) and stood at the entity origin. Owned here
        // so the borrowing `ExternalBModel` list can be built after the loop.
        let mut ext_cache: std::collections::HashMap<String, Option<Bsp>> = std::collections::HashMap::new();
        let mut ext_owned: Vec<(Bsp, [f32; 3])> = Vec::new();
        for e in 0..server.vm.num_edicts() {
            if server.vm.edict_free.get(e).copied().unwrap_or(true) || e as i32 == player {
                continue;
            }
            let ent = e as i32;
            // Only render entities with a real modelindex (setmodel ran). An edict
            // that early-returns before setmodel (e.g. a passable func_episodegate)
            // keeps its raw "*N" map key but no modelindex -> invisible in Quake.
            if server.vm.ent_get_float(ent, "modelindex") == 0.0 {
                continue;
            }
            let m = server.vm.ent_get_string(ent, "model");
            // Brush submodels (doors/plats/buttons) draw at the entity origin.
            if let Some(num) = m.strip_prefix('*') {
                if let Ok(idx) = num.parse::<usize>() {
                    let origin = server.vm.ent_get_vector(ent, "origin");
                    bmodels.push(render::BModelInstance { model_index: idx, origin, frame: server.vm.ent_get_float(ent, "frame") as i32 });
                }
                continue;
            }
            // External brush-model item box: a standalone b_*.bsp the item set as
            // its model (explosive box, ammo/health boxes), never the world map.
            if m.ends_with(".bsp") {
                if m != map_name {
                    if !ext_cache.contains_key(&m) {
                        ext_cache.insert(m.clone(), pak.read_file(&m).ok().flatten().and_then(|b| Bsp::parse(&b).ok()));
                    }
                    if let Some(Some(bsp)) = ext_cache.get(&m) {
                        let origin = server.vm.ent_get_vector(ent, "origin");
                        ext_owned.push((bsp.clone(), origin));
                    }
                }
                continue;
            }
            if !m.ends_with(".mdl") {
                continue;
            }
            if !model_cache.contains_key(&m) {
                model_cache.insert(m.clone(), pak.read_file(&m).ok().flatten().and_then(|b| Mdl::parse(&b).ok()));
            }
            if let Some(Some(mdl)) = model_cache.get(&m) {
                owned.push((
                    mdl.clone(),
                    server.vm.ent_get_vector(ent, "origin"),
                    server.vm.ent_get_vector(ent, "angles")[1],
                    color_for_name(&m),
                ));
            }
        }
        let inst: Vec<render::ModelInstance> = owned
            .iter()
            .map(|(mdl, origin, yaw, color)| render::ModelInstance { mdl, origin: *origin, yaw: *yaw, pitch: 0.0, roll: 0.0, color: *color, frame: 0, skinnum: 0 })
            .collect();
        let external: Vec<render::ExternalBModel> = ext_owned
            .iter()
            .map(|(bsp, origin)| render::ExternalBModel { bsp, origin: *origin })
            .collect();

        // The first-person weapon viewmodel: the player edict's `weaponmodel`
        // (e.g. "progs/v_shot.mdl") posed at its `weaponframe`. Loaded from the
        // pak like any other MDL and cached. Drawn anchored to the camera.
        let weapon_name = server.vm.ent_get_string(player, "weaponmodel");
        let weapon_frame = server.vm.ent_get_float(player, "weaponframe").max(0.0) as usize;
        let weapon_mdl: Option<Mdl> = if weapon_name.ends_with(".mdl") {
            pak.read_file(&weapon_name).ok().flatten().and_then(|b| Mdl::parse(&b).ok())
        } else {
            None
        };

        let (eye, a) = server.player_view();
        let cam = Camera { pos: eye, yaw: a[1], pitch: -a[0], roll: 0.0, fov_deg: 90.0 };
        let viewmodel = weapon_mdl
            .as_ref()
            .map(|mdl| render::Viewmodel {
                mdl,
                frame: weapon_frame,
                // No bob in this still; the default viewsize's fudge.
                origin_ofs: render::viewmodel_origin_ofs(&cam, 0.0, render::VIEWSIZE_DEFAULT),
            });
        // The live particles as (world pos, palette index) for the renderer; they
        // share the scene z-buffer so any behind a wall are hidden. Use the
        // peak-combat snapshot so the action shot actually shows the blood burst
        // (the end-of-combat pool is empty — the monster is dead by then).
        let parts: Vec<([f32; 3], u8)> = if peak_parts.is_empty() {
            particles.particles().iter().map(|p| (p.origin, p.color)).collect()
        } else {
            peak_parts.clone()
        };
        // The peak-combat dynamic lights so the action shot lights up the walls
        // near explosions / muzzle flashes (the end-of-combat pool is empty).
        // Pass the server clock so liquids warp and sky scrolls in the POV shot,
        // and the animated light-style scales so torches flicker and lights pulse.
        let light_styles = server.lightstyle_scales(server.time());
        let colormap = read("gfx/colormap.lmp").ok();
        let mut img = render::render_scene_ext(&bsp_render, &cam, 640, 400, &palette, &inst, &bmodels, &external, viewmodel, server.time(), &parts, &peak_dlights, &light_styles, colormap.as_deref());

        // Status bar (HUD) overlay: build a Hud from the player's stats and the
        // game's gfx.wad, then blit it on top of the finished 3-D frame. If
        // gfx.wad is missing or unparseable we just skip the overlay (the POV
        // shot still renders) rather than failing the whole command.
        if let Some(wad) = read("gfx.wad").ok().and_then(|b| Wad2::parse(b).ok()) {
            let stat = |f: &str| server.vm.ent_get_float(player, f) as i32;
            let hud = render::Hud {
                wad: &wad,
                palette: &palette,
                health: stat("health"),
                ammo: stat("currentammo"),
                armor: stat("armorvalue"),
                items: stat("items"),
                weapon: stat("weapon"),
                ammo_shells: stat("ammo_shells"),
                ammo_nails: stat("ammo_nails"),
                ammo_rockets: stat("ammo_rockets"),
                ammo_cells: stat("ammo_cells"),
                time: server.time(),
                monsters: 0,
                total_monsters: 0,
                secrets: 0,
                total_secrets: 0,
                level_name: "",
                show_scores: false,
                // The tool frames the full view under the bar (no viewsize).
                sb_lines: render::SB_LINES_FULL,
            };
            render::draw_hud_into(&mut img, &hud);
        }

        img.write_ppm(path).map_err(|e| format!("write {path}: {e}"))?;
        let _ = writeln!(
            o,
            "  rendered player POV -> {path}{}",
            if weapon_mdl.is_some() {
                format!(" (weapon viewmodel {weapon_name} frame {weapon_frame})")
            } else {
                String::new()
            }
        );
    }
    Ok(Out::Text(o))
}

/// `simbench <pak> <map.bsp> [frames]` — benchmark the GAME-LOGIC tick (no
/// rendering). Spawns the real map, connects a player, then runs `frames` server
/// frames of deterministic forward-walking input and reports the steady-state
/// per-frame cost of the simulation: `SV_Physics` (walk/push/toss), the QuakeC VM
/// (monster AI, item/door/trigger thinks), and BSP collision (`SV_Move`). This is
/// the sim counterpart to the `QUAKE_BENCH` render benchmark — together they cover
/// "not just rendering but the logic the game runs".
///
/// The first frames (cold field-offset cache, first monster sightings) are a
/// warm-up and excluded; the reported figure is the average over the timed window.
/// VM-statement and BSP-trace counts come from free-running counters
/// ([`Vm::stmt_count`], [`quake_rs::world::trace_count`]) so the breakdown is
/// exact, not sampled.
fn cmd_simbench(pak_path: &str, map_name: &str, frames: u32) -> Result<Out, String> {
    use std::time::Instant;
    let frames = frames.max(1);
    let pak = Pak::open(pak_path).map_err(|e| e.to_string())?;
    let read = |n: &str| -> Result<Vec<u8>, String> {
        pak.read_file(n).map_err(|e| e.to_string())?.ok_or_else(|| format!("{n} not found"))
    };
    let bsp_sim = Bsp::parse(&read(map_name)?).map_err(|e| e.to_string())?;
    let entities = bsp_sim.entities.clone();
    let progs = Progs::parse(&read("progs.dat")?).map_err(|e| e.to_string())?;

    let mut server = Server::with_pak(bsp_sim, progs, Some(pak.clone())).map_err(|e| e.to_string())?;
    server.set_map_name(map_name); // SV_SpawnServer: world.model + the mapname global
    let rep = server.spawn_entities().map_err(|e| e.to_string())?;
    let _player = server.connect_client().map_err(|e| format!("connect_client: {e}"))?;

    // Deterministic input: walk forward along the spawn yaw at full speed, the
    // same as a player holding W. dt = 0.1s is Quake's canonical server frame (the
    // monster think interval), so every frame fully exercises the AI tick — the
    // heaviest realistic per-frame logic load.
    let spawn_yaw = player_start(&entities).map(|(_, a)| a).unwrap_or(0.0);
    let cmd = UserCmd { forwardmove: 400.0, yaw: spawn_yaw, ..Default::default() };
    const DT: f32 = 0.1;

    // Warm-up: a handful of frames to populate the VM field-offset cache and let
    // the player settle onto the ground / nearby monsters notice it, so the timed
    // window measures steady state, not first-touch costs.
    let warmup = 20u32.min(frames);
    for _ in 0..warmup {
        let _ = server.client_frame(&cmd, DT).map_err(|e| format!("client_frame: {e}"))?;
    }

    // Timed window.
    let stmt0 = server.vm.stmt_count;
    quake_rs::world::reset_trace_count();
    let mut thinks = 0usize;
    let mut think_errors = 0usize;
    let start = Instant::now();
    for _ in 0..frames {
        let fr = server.client_frame(&cmd, DT).map_err(|e| format!("client_frame: {e}"))?;
        thinks += fr.thinks_fired;
        think_errors += fr.think_errors;
    }
    let elapsed = start.elapsed();
    let stmts = server.vm.stmt_count.wrapping_sub(stmt0);
    let traces = quake_rs::world::trace_count();

    // Live (non-free) edicts as a load proxy.
    let mut alive = 0usize;
    for e in 0..server.vm.num_edicts() {
        if !server.vm.edict_free.get(e).copied().unwrap_or(true) {
            alive += 1;
        }
    }

    let per_ms = elapsed.as_secs_f64() * 1000.0 / frames as f64;
    let game_secs = frames as f64 * DT as f64;
    let mut o = String::new();
    let _ = writeln!(
        o,
        "simbench {map_name}: {frames} frames @ dt={DT}s ({game_secs:.1}s game time), {} edicts spawned, {alive} alive",
        rep.spawned
    );
    let _ = writeln!(
        o,
        "  -> {per_ms:.4} ms/frame  ({:.0} sim-frames/sec)  | walked from info_player_start (yaw {spawn_yaw:.0})",
        1000.0 / per_ms
    );
    let _ = writeln!(
        o,
        "  per frame: {:.0} VM statements, {:.1} BSP traces, {:.1} thinks{}",
        stmts as f64 / frames as f64,
        traces as f64 / frames as f64,
        thinks as f64 / frames as f64,
        if think_errors > 0 { format!(" ({think_errors} think errors)") } else { String::new() },
    );
    let _ = writeln!(
        o,
        "  totals: {stmts} VM statements, {traces} traces, {thinks} thinks over the window",
    );
    Ok(Out::Text(o))
}

/// `changelevel <pak> <map.bsp>`: boot the map, find its `trigger_changelevel`
/// exit, drive the player onto it until the QuakeC `changelevel()` builtin fires
/// (recording the next map), then perform the engine-side swap — save the
/// player's spawn parms, load the next BSP + a fresh progs, spawn the next
/// level, and reconnect the client carrying its inventory. Reports the next map,
/// the entities spawned there, and the player's weapon/items/health/armor BEFORE
/// vs AFTER the swap to prove the inventory carried across.
fn cmd_changelevel(pak_path: &str, map_name: &str) -> Result<Out, String> {
    let pak = Pak::open(pak_path).map_err(|e| e.to_string())?;
    let read = |n: &str| -> Result<Vec<u8>, String> {
        pak.read_file(n).map_err(|e| e.to_string())?.ok_or_else(|| format!("{n} not found"))
    };
    let bsp_render = Bsp::parse(&read(map_name)?).map_err(|e| e.to_string())?;
    let bsp_sim = Bsp::parse(&read(map_name)?).map_err(|e| e.to_string())?;
    let progs = Progs::parse(&read("progs.dat")?).map_err(|e| e.to_string())?;

    let mut server = Server::with_pak(bsp_sim, progs, Some(pak.clone())).map_err(|e| e.to_string())?;
    server.set_map_name(map_name); // SV_SpawnServer: world.model + the mapname global
    let rep = server.spawn_entities().map_err(|e| e.to_string())?;
    let player = server.connect_client().map_err(|e| format!("connect_client: {e}"))?;

    let mut o = String::new();
    let _ = writeln!(
        o,
        "changelevel {map_name}: {} entities spawned; player = edict {player}",
        rep.spawned
    );

    // A small snapshot of the player's persistent state for the BEFORE/AFTER
    // comparison: weapon, items bitfield, health, armor, and the three ammo
    // counters DecodeLevelParms restores.
    let snapshot = |s: &Server| -> [i64; 7] {
        let g = |f: &str| s.vm.ent_get_float(player.max(0), f) as i64;
        [
            g("weapon"),
            g("items"),
            g("health"),
            g("armorvalue"),
            g("ammo_shells"),
            g("ammo_nails"),
            g("ammo_rockets"),
        ]
    };
    let fmt = |v: &[i64; 7]| {
        format!(
            "weapon={} items={:#x} health={} armor={} shells={} nails={} rockets={}",
            v[0], v[1], v[2], v[3], v[4], v[5], v[6]
        )
    };

    // Locate the trigger_changelevel exit. The brush trigger has no `origin`; it
    // occupies a brush volume, so read the absmin/absmax setmodel + link gave it
    // and aim for the box centre. Also pull its `map` key (the expected next map).
    let mut trigger: Option<(i32, [f32; 3])> = None;
    for e in 0..server.vm.num_edicts() {
        let ent = e as i32;
        if server.vm.edict_free.get(e).copied().unwrap_or(true) {
            continue;
        }
        if server.vm.ent_get_string(ent, "classname") == "trigger_changelevel" {
            let amin = server.vm.ent_get_vector(ent, "absmin");
            let amax = server.vm.ent_get_vector(ent, "absmax");
            let centre = [
                0.5 * (amin[0] + amax[0]),
                0.5 * (amin[1] + amax[1]),
                0.5 * (amin[2] + amax[2]),
            ];
            trigger = Some((ent, centre));
            break;
        }
    }
    let Some((trig, centre)) = trigger else {
        return Err(format!("{map_name} has no trigger_changelevel"));
    };
    let trig_map = trigger_map_key(&bsp_render.entities);
    let _ = writeln!(
        o,
        "  found trigger_changelevel (edict {trig}) targeting map \"{}\"; volume centre {centre:?}",
        trig_map.as_deref().unwrap_or("?")
    );

    // Drive the player into the trigger volume: teleport onto the centre, then
    // tick frames (the touch fires during SV_Physics_Client) until the deferred
    // changelevel() request appears. A small jiggle of forward motion settles the
    // box so its absmin/absmax overlaps the trigger and SV_TouchLinks fires.
    // Drive the player into the trigger volume: hold it on the centre for a few
    // frames so SV_TouchLinks fires the trigger's `changelevel_touch`. In stock
    // single-player that touch sets the `nextmap` global and starts the
    // end-of-level intermission, whose exit (a player button press in-game) runs
    // `GotoNextMap` -> `changelevel(nextmap)`. We drive the touch by ticking
    // frames, then — if the deferred builtin has not fired yet — run the canonical
    // `GotoNextMap` exit function (self = player) to issue the changelevel, the
    // same call the intermission exit makes. Either way the deferred request
    // appears for `take_pending_changelevel`.
    let spawn_yaw = player_start(&bsp_render.entities).map(|(_, a)| a).unwrap_or(0.0);
    let cmd = UserCmd { yaw: spawn_yaw, ..Default::default() };
    let mut requested: Option<String> = None;
    let mut frames_driven = 0u32;
    for f in 0..40 {
        frames_driven = f + 1;
        // Pin the player in the volume so its box overlaps the trigger and the
        // touch fires (SV_TouchLinks runs during SV_Physics_Client).
        server.vm.ent_set_vector(player, "origin", centre);
        server.vm.ent_set_vector(player, "velocity", [0.0, 0.0, 0.0]);
        server.client_frame(&cmd, 0.1).map_err(|e| format!("client_frame: {e}"))?;
        if let Some(m) = server.take_pending_changelevel() {
            requested = Some(m);
            break;
        }
        // Once the touch has set `nextmap` (the intermission is now armed), run the
        // intermission-exit function the same way a player button press would,
        // which calls the changelevel() builtin.
        if requested.is_none() && server.vm.gget_int("nextmap") != 0 {
            if let Some(goto) = server.vm.progs.find_function("GotoNextMap") {
                server.vm.gset_int("self", player);
                server.vm.gset_int("other", 0);
                if server.vm.execute(goto).is_err() {
                    server.vm.reset_execution();
                }
            }
            if let Some(m) = server.take_pending_changelevel() {
                requested = Some(m);
                break;
            }
        }
    }
    let Some(next_map_name) = requested else {
        return Err(format!(
            "player never triggered changelevel() (no request after {frames_driven} frames; nextmap global = {})",
            server.vm.gget_int("nextmap")
        ));
    };
    let _ = writeln!(o, "  drove player into the exit ({frames_driven} frames)");
    // QuakeC stores the bare map name ("e1m2"); the BSP lives at "maps/<name>.bsp".
    let _ = writeln!(o, "  changelevel() fired -> next map \"{next_map_name}\"");

    // Snapshot the inventory BEFORE the swap, then save the spawn parms (this runs
    // the QuakeC SetChangeParms, marshalling the player's state into parm1..16).
    let before = snapshot(&server);
    let parms = server.save_spawn_parms();
    let _ = writeln!(o, "  BEFORE swap: {}", fmt(&before));
    let _ = writeln!(
        o,
        "  saved spawn parms: {}",
        parms.iter().map(|p| format!("{p:.0}")).collect::<Vec<_>>().join(",")
    );

    // Build the next server from the next BSP + a fresh progs, spawn its entities,
    // and reconnect the client carrying the saved parms (DecodeLevelParms restores
    // the inventory inside PutClientInServer).
    let next_bsp_path = format!("maps/{next_map_name}.bsp");
    let next_bytes = read(&next_bsp_path)
        .map_err(|e| format!("loading next map {next_bsp_path}: {e}"))?;
    let next_bsp = Bsp::parse(&next_bytes).map_err(|e| e.to_string())?;
    let next_progs = Progs::parse(&read("progs.dat")?).map_err(|e| e.to_string())?;
    // Carry the chosen difficulty across (skill is a thread-local that with_pak
    // resets to 1) — captured before building next_server, restored after.
    let carry_skill = server.skill();
    let mut next_server = Server::with_pak(next_bsp, next_progs, Some(pak.clone())).map_err(|e| e.to_string())?;
    next_server.set_map_name(&next_map_name); // SV_SpawnServer for the swapped-to level
    next_server.set_skill(carry_skill as f32);
    let next_rep = next_server.spawn_entities().map_err(|e| e.to_string())?;
    let next_player = next_server
        .connect_client_with_parms(parms)
        .map_err(|e| format!("connect_client_with_parms: {e}"))?;
    let _ = writeln!(
        o,
        "  swapped to {next_bsp_path}: {} entities spawned; player = edict {next_player}",
        next_rep.spawned
    );

    let after_player = next_player;
    let after = {
        let g = |f: &str| next_server.vm.ent_get_float(after_player, f) as i64;
        [
            g("weapon"),
            g("items"),
            g("health"),
            g("armorvalue"),
            g("ammo_shells"),
            g("ammo_nails"),
            g("ammo_rockets"),
        ]
    };
    let _ = writeln!(o, "  AFTER swap:  {}", fmt(&after));
    let carried = after[0] == before[0]
        && after[1] == before[1]
        && after[4] == before[4]
        && after[5] == before[5]
        && after[6] == before[6];
    let _ = writeln!(
        o,
        "  inventory carried: {}  (weapon/items/ammo match across the swap)",
        if carried { "YES" } else { "NO" }
    );
    Ok(Out::Text(o))
}

/// Read the `map` key from the first `trigger_changelevel` block in an entity
/// lump (the bare destination map name, e.g. `"e1m2"`).
fn trigger_map_key(ents: &str) -> Option<String> {
    for block in ents.split('}') {
        let toks: Vec<&str> = block.split('"').collect();
        let mut classname = "";
        let mut map = None;
        let mut i = 1;
        while i + 2 < toks.len() {
            match toks[i] {
                "classname" => classname = toks[i + 2],
                "map" => map = Some(toks[i + 2].trim().to_string()),
                _ => {}
            }
            i += 4;
        }
        if classname == "trigger_changelevel" {
            return map;
        }
    }
    None
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

fn cmd_dis(path: &str) -> Result<Out, String> {
    let bytes = read(path)?;
    let p = Progs::parse(&bytes).map_err(|e| e.to_string())?;
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

fn cmd_run(path: &str, func: &str) -> Result<Out, String> {
    let bytes = read(path)?;
    let mut vm = Vm::load(&bytes).map_err(|e| e.to_string())?;
    vm.call_by_name(func).map_err(|e| e.to_string())?;
    let mut o = String::new();
    if !vm.output.is_empty() {
        let _ = writeln!(o, "--- output ---");
        o.push_str(vm.output.trim_end());
        o.push('\n');
    }
    let _ = writeln!(
        o,
        "--- {func}() returned: float={} int={} ---",
        vm.gf(OFS_RETURN),
        vm.gi(OFS_RETURN)
    );
    Ok(Out::Text(o))
}

// ----------------------------------------------------------- software render ---

/// Find `info_player_start`'s origin and angle from the entity lump.
fn player_start(ents: &str) -> Option<([f32; 3], f32)> {
    for block in ents.split('}') {
        // collect "key" "value" pairs in this entity block
        let toks: Vec<&str> = block.split('"').collect();
        let mut classname = "";
        let mut origin = None;
        let mut angle = 0.0f32;
        let mut i = 1;
        while i + 2 < toks.len() {
            let key = toks[i];
            let val = toks[i + 2];
            match key {
                "classname" => classname = val,
                "origin" => {
                    let n: Vec<f32> = val.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                    if n.len() == 3 {
                        origin = Some([n[0], n[1], n[2]]);
                    }
                }
                "angle" => angle = val.trim().parse().unwrap_or(0.0),
                _ => {}
            }
            i += 4; // step over "key" <sep> "value" <sep>
        }
        if classname == "info_player_start" {
            if let Some(o) = origin {
                return Some((o, angle));
            }
        }
    }
    None
}

/// Camera at the player spawn (eye height +24), facing the spawn angle; falls
/// back to the centre of the map bounds looking +X if there's no spawn.
fn camera_for_bsp(b: &Bsp) -> Camera {
    if let Some((o, ang)) = player_start(&b.entities) {
        let eye = [o[0], o[1], o[2] + 24.0];
        let yaw = ang.to_radians();
        let target = [eye[0] + yaw.cos(), eye[1] + yaw.sin(), eye[2]];
        return Camera::looking_at(eye, target, 90.0);
    }
    let (mut mn, mut mx) = ([f32::MAX; 3], [f32::MIN; 3]);
    if let Some(m) = b.models.first() {
        mn = m.mins;
        mx = m.maxs;
    } else {
        for v in &b.vertexes {
            for i in 0..3 {
                mn[i] = mn[i].min(v.point[i]);
                mx[i] = mx[i].max(v.point[i]);
            }
        }
    }
    let c = [(mn[0] + mx[0]) * 0.5, (mn[1] + mx[1]) * 0.5, (mn[2] + mx[2]) * 0.5];
    Camera::looking_at(c, [c[0] + 1.0, c[1], c[2]], 90.0)
}

fn cmd_render(path: &str, out: &str, palette: Option<&str>) -> Result<Out, String> {
    let bytes = read(path)?;
    let b = Bsp::parse(&bytes).map_err(|e| e.to_string())?;
    let cam = camera_for_bsp(&b);
    let (img, mode) = match palette {
        Some(pp) => {
            let pbytes = read(pp)?;
            let pal = render::parse_palette(&pbytes)
                .ok_or_else(|| format!("bad palette {pp} (need >= 768 bytes)"))?;
            (render::render_bsp_textured(&b, &cam, 640, 400, &pal), "textured")
        }
        None => (render::render_bsp(&b, &cam, 640, 400), "flat-shaded"),
    };
    img.write_ppm(out).map_err(|e| format!("cannot write {out}: {e}"))?;
    Ok(Out::Text(format!(
        "rendered {} faces of {path} ({mode}) -> {out} ({}x{} PPM)\n",
        b.faces.len(),
        img.w,
        img.h
    )))
}

fn cmd_render_demo(out: &str) -> Result<Out, String> {
    let b = render::demo_room();
    // Inside the room, off a corner, looking toward the centre/pillar.
    let cam = Camera::looking_at([-200.0, -200.0, 40.0], [0.0, 0.0, 0.0], 90.0);
    let img = render::render_bsp(&b, &cam, 640, 400);
    img.write_ppm(out).map_err(|e| format!("cannot write {out}: {e}"))?;
    Ok(Out::Text(format!(
        "rendered demo room ({} faces) -> {out} ({}x{} PPM)\n",
        b.faces.len(),
        img.w,
        img.h
    )))
}

/// `menu <pak> <out.ppm>`: boot e1m1 from the pak, software-render its POV, draw
/// the iconic MAIN menu (a port of `M_Main_Draw`) over it, and write the PPM —
/// the menu can then be eyeballed. A missing pak/map/palette is a clean error;
/// any missing menu pic is skipped (the rest still draws), and if the POV cannot
/// render the menu is drawn over a black frame instead so the command never fails
/// just because the world didn't load.
fn cmd_menu(pak_path: &str, out: &str) -> Result<Out, String> {
    let pak = Pak::open(pak_path).map_err(|e| e.to_string())?;
    let read = |n: &str| -> Result<Vec<u8>, String> {
        pak.read_file(n).map_err(|e| e.to_string())?.ok_or_else(|| format!("{n} not found"))
    };

    // Palette is required to colour both the world and the menu pics.
    let palette = render::parse_palette(&read("gfx/palette.lmp")?)
        .ok_or("bad palette (need >= 768 bytes)")?;

    // The POV background: render e1m1 from the player spawn if we can; otherwise
    // fall back to a black 320x200 frame (the menu is the point of this command).
    const W: usize = 320;
    const H: usize = 200;
    let (mut img, bg) = match read("maps/e1m1.bsp").ok().and_then(|b| Bsp::parse(&b).ok()) {
        Some(b) => {
            let cam = camera_for_bsp(&b);
            (render::render_bsp_textured(&b, &cam, W, H, &palette), "e1m1 POV")
        }
        None => (render::Image::new(W, H, [0, 0, 0]), "black frame"),
    };

    // Load the menu pics from the pak's .lmp files (each optional) + conchars
    // (raw 128x128 block) from gfx.wad.
    let lmp = |n: &str| -> Option<quake_rs::wad::Qpic> {
        pak.read_file(n).ok().flatten().and_then(|b| quake_rs::wad::Qpic::parse(&b).ok())
    };
    let mut menudot: [Option<quake_rs::wad::Qpic>; 6] = Default::default();
    for (i, slot) in menudot.iter_mut().enumerate() {
        *slot = lmp(&format!("gfx/menudot{}.lmp", i + 1));
    }
    let mut help: [Option<quake_rs::wad::Qpic>; render::NUM_HELP_PAGES] = Default::default();
    for (i, slot) in help.iter_mut().enumerate() {
        *slot = lmp(&format!("gfx/help{i}.lmp"));
    }
    let present = |o: &Option<quake_rs::wad::Qpic>| o.is_some();
    let pics = render::MenuPics {
        qplaque: lmp("gfx/qplaque.lmp"),
        ttl_main: lmp("gfx/ttl_main.lmp"),
        mainmenu: lmp("gfx/mainmenu.lmp"),
        ttl_sgl: lmp("gfx/ttl_sgl.lmp"),
        sp_menu: lmp("gfx/sp_menu.lmp"),
        p_option: lmp("gfx/p_option.lmp"),
        p_load: lmp("gfx/p_load.lmp"),
        p_save: lmp("gfx/p_save.lmp"),
        p_multi: lmp("gfx/p_multi.lmp"),
        mp_menu: lmp("gfx/mp_menu.lmp"),
        ttl_cstm: lmp("gfx/ttl_cstm.lmp"),
        vidmodes: lmp("gfx/vidmodes.lmp"),
        menudot,
        help,
    };
    let conchars = read("gfx.wad").ok().and_then(|b| Wad2::parse(b).ok()).and_then(|w| {
        let lump = w.lump("conchars")?;
        let data = w.lump_data(lump).ok()?;
        if data.len() < 128 * 128 {
            return None;
        }
        Some(quake_rs::wad::Qpic { width: 128, height: 128, data: data[..128 * 128].to_vec() })
    });

    // Open the MAIN menu and draw it over the POV (host_time fixed so the cursor
    // frame is reproducible).
    let mut menu = render::Menu::new();
    menu.open();
    render::draw_menu(&mut img, &menu, &pics, conchars.as_ref(), 0.0, 0.0, &palette);

    img.write_ppm(out).map_err(|e| format!("cannot write {out}: {e}"))?;

    let mut o = String::new();
    let _ = writeln!(o, "drew the MAIN menu over the {bg} -> {out} ({}x{} PPM)", img.w, img.h);
    let _ = writeln!(
        o,
        "  pics: qplaque={} ttl_main={} mainmenu={} menudot={}/6 conchars={}",
        present(&pics.qplaque),
        present(&pics.ttl_main),
        present(&pics.mainmenu),
        pics.menudot.iter().filter(|d| d.is_some()).count(),
        conchars.is_some()
    );
    Ok(Out::Text(o))
}

/// A vivid, deterministic RGB colour derived from a model name. (We don't reuse
/// `render::hash_color` — it's private — so this mirrors its spirit with a small
/// name hash mapped through a saturated HSV-ish ramp.)
fn color_for_name(name: &str) -> [u8; 3] {
    // FNV-1a over the bytes for a stable spread across names.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in name.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    // Hue in [0,6); fixed high saturation/value for vivid colours.
    let hue6 = ((h & 0xFFFF) as f32 / 65536.0) * 6.0;
    let i = hue6 as i32; // 0..=5
    let f = hue6 - i as f32;
    let (v, p) = (255.0f32, 60.0f32);
    let q = v - (v - p) * f;
    let t = p + (v - p) * f;
    let to = |x: f32| x.clamp(0.0, 255.0) as u8;
    match i {
        0 => [to(v), to(t), to(p)],
        1 => [to(q), to(v), to(p)],
        2 => [to(p), to(v), to(t)],
        3 => [to(p), to(q), to(v)],
        4 => [to(t), to(p), to(v)],
        _ => [to(v), to(p), to(q)],
    }
}

/// `scene`: parse a map from a PAK, spawn its QuakeC entities, and software-
/// render the world plus every spawned entity's `.mdl` alias model at its world
/// position, all sharing one z-buffer so models occlude correctly.
fn cmd_scene(pak_path: &str, map_name: &str, out: &str) -> Result<Out, String> {
    use std::collections::HashMap;

    let pak = Pak::open(pak_path).map_err(|e| e.to_string())?;

    // --- map BSP bytes from the pak (parsed twice: render + sim) ---
    let bsp_bytes = pak
        .read_file(map_name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("{map_name:?} not found in {pak_path}"))?;
    let bsp_for_render = Bsp::parse(&bsp_bytes).map_err(|e| e.to_string())?;
    let bsp_for_sim = Bsp::parse(&bsp_bytes).map_err(|e| e.to_string())?;

    // --- palette + progs from the pak (graceful errors, never panic) ---
    let pal_bytes = pak
        .read_file("gfx/palette.lmp")
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("gfx/palette.lmp not found in {pak_path}"))?;
    let palette = render::parse_palette(&pal_bytes)
        .ok_or_else(|| format!("bad palette in {pak_path} (need >= 768 bytes)"))?;

    let progs_bytes = pak
        .read_file("progs.dat")
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("progs.dat not found in {pak_path}"))?;
    let progs = Progs::parse(&progs_bytes).map_err(|e| e.to_string())?;

    // Camera from the player start (derived from the render BSP before sim).
    // Eye at the player spawn; we re-aim it at the nearest model once we know
    // where the entities are, so a monster/item is framed instead of a wall.
    let base_cam = camera_for_bsp(&bsp_for_render);
    let eye = base_cam.pos;

    // --- spawn the map's entities ---
    let mut server = Server::with_pak(bsp_for_sim, progs, Some(pak.clone())).map_err(|e| e.to_string())?;
    server.set_map_name(map_name); // SV_SpawnServer: world.model + the mapname global
    let report = server.spawn_entities().map_err(|e| e.to_string())?;

    // --- gather MDL instances from live edicts ---
    // Cache parsed models by in-pak name so each loads at most once.
    let mut model_cache: HashMap<String, Option<Mdl>> = HashMap::new();
    // Owned model data outlives the borrowing ModelInstances below.
    let mut owned: Vec<(Mdl, [f32; 3], f32, [u8; 3])> = Vec::new();
    let mut monster_origins: Vec<[f32; 3]> = Vec::new();
    let mut bmodels: Vec<render::BModelInstance> = Vec::new();
    // External brush-model item boxes (maps/b_*.bsp): parsed once per name and
    // stood at the entity origin. Owned so the borrowing `ExternalBModel` list can
    // be built after the loop (same pattern as `owned` for MDLs).
    let mut ext_cache: HashMap<String, Option<Bsp>> = HashMap::new();
    let mut ext_owned: Vec<(Bsp, [f32; 3])> = Vec::new();
    let mut skipped_load = 0usize;

    let n = server.vm.num_edicts();
    for e in 0..n {
        // Skip free edicts (and the implicit world at 0 has no .mdl model).
        if server.vm.edict_free.get(e).copied().unwrap_or(true) {
            continue;
        }
        let ent = e as i32;
        // Only render entities whose QuakeC spawn actually setmodel'd (modelindex
        // != 0); a passable func_episodegate keeps its "*N" map key but no
        // modelindex and is invisible in Quake.
        if server.vm.ent_get_float(ent, "modelindex") == 0.0 {
            continue;
        }
        let model = server.vm.ent_get_string(ent, "model");
        if model.is_empty() {
            continue;
        }
        // Brush submodels ("*N") — doors, platforms, buttons — draw as bmodels at
        // the entity origin (the world pass only draws model 0).
        if let Some(num) = model.strip_prefix('*') {
            if let Ok(idx) = num.parse::<usize>() {
                let origin = server.vm.ent_get_vector(ent, "origin");
                bmodels.push(render::BModelInstance { model_index: idx, origin, frame: server.vm.ent_get_float(ent, "frame") as i32 });
            }
            continue;
        }
        // External brush-model item box (maps/b_*.bsp): a standalone bsp the item
        // set as its model (explosive box, ammo/health boxes). The world map path
        // itself is never an entity model here (worldspawn is the `*0` branch), but
        // guard against it explicitly so the world is never re-drawn as a box.
        if model.ends_with(".bsp") {
            if model != map_name {
                if !ext_cache.contains_key(&model) {
                    let parsed = match pak.read_file(&model) {
                        Ok(Some(bytes)) => Bsp::parse(&bytes).ok(),
                        _ => None,
                    };
                    ext_cache.insert(model.clone(), parsed);
                }
                if let Some(Some(bsp)) = ext_cache.get(&model) {
                    let origin = server.vm.ent_get_vector(ent, "origin");
                    ext_owned.push((bsp.clone(), origin));
                }
            }
            continue;
        }
        if model.starts_with("maps/") {
            continue;
        }
        if !model.ends_with(".mdl") {
            continue;
        }

        // Load (and cache) the parsed model.
        if !model_cache.contains_key(&model) {
            let parsed = match pak.read_file(&model) {
                Ok(Some(bytes)) => Mdl::parse(&bytes).ok(),
                _ => None,
            };
            model_cache.insert(model.clone(), parsed);
        }
        let Some(Some(mdl)) = model_cache.get(&model) else {
            skipped_load += 1;
            continue;
        };

        let origin = server.vm.ent_get_vector(ent, "origin");
        let angles = server.vm.ent_get_vector(ent, "angles");
        let yaw = angles[1]; // angles = [pitch, yaw, roll]
        let color = color_for_name(&model);
        if server.vm.ent_get_string(ent, "classname").starts_with("monster") {
            monster_origins.push(origin);
        }
        owned.push((mdl.clone(), origin, yaw, color));
    }

    let instances: Vec<render::ModelInstance> = owned
        .iter()
        .map(|(mdl, origin, yaw, color)| render::ModelInstance {
            mdl,
            origin: *origin,
            yaw: *yaw, pitch: 0.0, roll: 0.0,
            color: *color,
            frame: 0,
            skinnum: 0,
        })
        .collect();
    let external: Vec<render::ExternalBModel> = ext_owned
        .iter()
        .map(|(bsp, origin)| render::ExternalBModel { bsp, origin: *origin })
        .collect();

    // Aim the camera from the spawn eye at the nearest model that is not almost
    // on top of us (so it's framed, not degenerate); fall back to the spawn view.
    let d2 = |a: [f32; 3], b: [f32; 3]| {
        let (dx, dy, dz) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
        dx * dx + dy * dy + dz * dz
    };
    // Prefer the nearest monster (bigger, more recognisable); else nearest model.
    let pick = |pts: &[[f32; 3]]| {
        pts.iter()
            .copied()
            .filter(|o| d2(*o, eye) > 64.0 * 64.0)
            .min_by(|a, b| d2(*a, eye).total_cmp(&d2(*b, eye)))
    };
    let model_pts: Vec<[f32; 3]> = owned.iter().map(|(_, o, _, _)| *o).collect();
    // When QUAKE_AIM_DOOR is set, frame the nearest brush submodel (door/plat) so
    // the bmodel render can be eyeballed; otherwise frame the nearest monster.
    // A door's entity origin is usually [0,0,0] (the brush geometry carries the
    // position), so aim at the centre of model N's bounds + the entity origin.
    let door_pts: Vec<[f32; 3]> = bmodels
        .iter()
        .filter_map(|b| {
            let m = bsp_for_render.models.get(b.model_index)?;
            Some([
                b.origin[0] + (m.mins[0] + m.maxs[0]) * 0.5,
                b.origin[1] + (m.mins[1] + m.maxs[1]) * 0.5,
                b.origin[2] + (m.mins[2] + m.maxs[2]) * 0.5,
            ])
        })
        .collect();
    let target = if std::env::var("QUAKE_AIM_DOOR").is_ok() {
        pick(&door_pts).or_else(|| pick(&monster_origins)).or_else(|| pick(&model_pts))
    } else {
        pick(&monster_origins).or_else(|| pick(&model_pts))
    };
    let cam = match target {
        Some(t) => {
            // Stand ~110 units in front of the target (between it and the eye),
            // at its mid-height, looking at it — so it fills a good part of frame.
            let dist = d2(t, eye).sqrt();
            let f = if dist > 130.0 { (dist - 110.0) / dist } else { 0.0 };
            let pos = [
                eye[0] + (t[0] - eye[0]) * f,
                eye[1] + (t[1] - eye[1]) * f,
                eye[2] + (t[2] - eye[2]) * f,
            ];
            Camera::looking_at(pos, [t[0], t[1], t[2] + 16.0], 90.0)
        }
        None => base_cam,
    };

    // Pass the server clock so liquid/sky surfaces are animated for this frame,
    // plus the animated light-style scales (torch flicker / light pulse). No live
    // particles in this single-shot `scene` command (no per-frame loop), so that
    // slice is empty; dynamic lights are empty too unless explicitly injected
    // below for A/B debugging.
    let light_styles = server.lightstyle_scales(server.time());

    // Optional injected dynamic light, for eyeballing / A-B-diffing the dlight
    // path (e.g. the R_MarkLights BSP gating) on a real map:
    //   QUAKE_DLIGHT="x,y,z,radius" quaketool scene <pak> <map> <out>
    //   QUAKE_DLIGHT="eye"          (at the camera, radius 350 — explosion-sized)
    //   QUAKE_DLIGHT="eye:250"      (at the camera, radius 250)
    // Unset (the normal case, and all golden renders) leaves the dlight slice
    // empty — byte-identical to before this knob existed.
    let injected_dlights: Vec<quake_rs::dlight::DynamicLight> = std::env::var("QUAKE_DLIGHT")
        .ok()
        .and_then(|s| {
            let s = s.trim().to_string();
            let (origin, radius) = if let Some(rest) = s.strip_prefix("eye") {
                let r = rest.strip_prefix(':').and_then(|r| r.parse().ok()).unwrap_or(350.0);
                (cam.pos, r)
            } else {
                let v: Vec<f32> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                if v.len() != 4 {
                    return None;
                }
                ([v[0], v[1], v[2]], v[3])
            };
            // die far in the future / no decay: the light is fully live for this
            // single frame. key 0 = unowned (explosion-style).
            Some(vec![quake_rs::dlight::DynamicLight::new(origin, radius, f32::MAX, 0.0, 0.0, 0)])
        })
        .unwrap_or_default();
    // Read the colormap from the PAK (not the filesystem), matching the live game,
    // so this single-shot render uses id's 64-row colormap-LUT shading (and the lit
    // surface cache) exactly like step_walk does.
    let colormap = pak.read_file("gfx/colormap.lmp").ok().flatten();

    // Optional render benchmark, reusing this command's full scene setup:
    //   QUAKE_BENCH=<iters> [QUAKE_RES=<WxH>] quaketool scene <pak> <map> <out>
    // Renders the scene `iters` times at the given resolution and reports the WARM
    // per-frame cost (the first, cache-cold frame is excluded). This exercises the
    // exact rasteriser the live game uses, so it measures the real render cost.
    if let Ok(iters) = std::env::var("QUAKE_BENCH") {
        let iters: u32 = iters.parse().unwrap_or(60).max(1);
        let (bw, bh) = std::env::var("QUAKE_RES")
            .ok()
            .and_then(|s| {
                let mut it = s.split(['x', 'X']);
                Some((it.next()?.trim().parse().ok()?, it.next()?.trim().parse().ok()?))
            })
            .unwrap_or((640usize, 400usize));
        let render_once = || {
            render::render_scene_ext(
                &bsp_for_render, &cam, bw, bh, &palette, &instances, &bmodels, &external, None,
                server.time(), &[], &injected_dlights, &light_styles, colormap.as_deref(),
            )
        };
        let _ = std::hint::black_box(render_once()); // warm the per-face caches
        let start = std::time::Instant::now();
        for _ in 0..iters {
            std::hint::black_box(render_once());
        }
        let per = start.elapsed().as_secs_f64() * 1000.0 / iters as f64;

        // One profiled frame (caches already warm) for the per-phase breakdown.
        render::render_stats_begin();
        let _ = std::hint::black_box(render_once());
        let st = render::render_stats_end();
        let ms = |ns: u64| ns as f64 / 1_000_000.0;
        let mut o = String::new();
        use std::fmt::Write as _;
        let _ = writeln!(
            o,
            "bench {map_name} {bw}x{bh}: {iters} warm frames -> {per:.2} ms/frame ({:.1} fps)",
            1000.0 / per
        );
        let _ = writeln!(
            o,
            "  phases (ms): world {:.2}  submodel {:.2}  external {:.2}  alias {:.2}  particle {:.2}  sprite {:.2}  viewmodel {:.2}",
            ms(st.world_ns), ms(st.submodel_ns), ms(st.external_ns), ms(st.alias_ns),
            ms(st.particle_ns), ms(st.sprite_ns), ms(st.viewmodel_ns),
        );
        let _ = writeln!(
            o,
            "  world: {} faces ({} pvs-cull, {} frustum-cull, {} drawn), {} tris, {} px, surf {}/{} hit/miss",
            st.faces_total, st.faces_pvs_culled, st.faces_frustum_culled, st.faces_drawn,
            st.world_tris, st.world_pixels, st.surf_hits, st.surf_misses,
        );
        let _ = writeln!(
            o,
            "  submodel: {} faces visited, {} drawn ({}/{} surf hit/miss), {} tris, {} lightmap rebuilds",
            st.sub_faces_visited, st.sub_faces_drawn, st.sub_surf_hits, st.sub_surf_misses,
            st.sub_tris, st.sub_lm_builds,
        );
        let _ = writeln!(
            o,
            "  world sub-phases (ms): pvs {:.2}  sort {:.2}  setup+raster {:.2}  lightmap {:.2}  surf {:.2}",
            ms(st.world_pvs_ns), ms(st.world_sort_ns), ms(st.world_setup_ns),
            ms(st.world_light_ns), ms(st.world_surf_ns),
        );
        let _ = writeln!(
            o,
            "  surf cache: {} true-hits, {} cached-rebakes (warm: should be ~0), {} external-bypass-bakes (expected, cheap)",
            st.surf_cache_hits, st.surf_baked, st.surf_bypass_baked,
        );
        return Ok(Out::Text(o));
    }

    let img = render::render_scene_ext(&bsp_for_render, &cam, 640, 400, &palette, &instances, &bmodels, &external, None, server.time(), &[], &injected_dlights, &light_styles, colormap.as_deref());
    img.write_ppm(out).map_err(|e| format!("cannot write {out}: {e}"))?;

    let mut o = String::new();
    let _ = writeln!(
        o,
        "scene {map_name} from {pak_path}: {} faces, {} entities spawned",
        bsp_for_render.faces.len(),
        report.spawned
    );
    if let Some(dl) = injected_dlights.first() {
        let _ = writeln!(
            o,
            "  injected dlight at [{:.0} {:.0} {:.0}] radius {:.0} (camera at [{:.0} {:.0} {:.0}])",
            dl.origin[0], dl.origin[1], dl.origin[2], dl.radius,
            cam.pos[0], cam.pos[1], cam.pos[2]
        );
    }
    let _ = writeln!(
        o,
        "  {} MDL instances drawn ({} unique models, {} failed to load)",
        instances.len(),
        model_cache.values().filter(|v| v.is_some()).count(),
        skipped_load
    );
    let _ = writeln!(o, "  -> {out} ({}x{} PPM)", img.w, img.h);
    Ok(Out::Text(o))
}

/// `view <pak> <map.bsp> <out.ppm> [options]` — render ONE exactly specified view,
/// so the C oracle (`oracle/`: id's own software renderer, headless) can be diffed
/// against this port pixel for pixel. The camera is given in Quake's convention —
/// `r_refdef.vieworg` and `r_refdef.viewangles` (pitch positive looks DOWN) — and
/// the clock is explicit, so light styles, sky scroll, liquid turb and texture
/// animation sit at the same phase as the C frame.
///
/// ```text
/// --res WxH          framebuffer size (default 320x200)
/// --origin x,y,z     eye position (default info_player_start + 22, the view height)
/// --angles p,y,r     view angles in degrees (default 0,<start angle>,0)
/// --time T           the render clock, cl.time (default: the server clock after spawn)
/// --fov F            horizontal field of view (default 90)
/// --ents FILE        draw these entities: the oracle's `.ents` list, one per line,
///                    `model ox oy oz pitch yaw roll frame skin syncbase effects kind`
///                    (without it: the world only, as r_drawentities 0)
/// --viewmodel M:F    also draw weapon model M at frame F
/// --bench N          then render the same view N more times, report warm ms/frame
/// ```
///
/// The map's entities are still spawned (worldspawn's QuakeC sets the light-style
/// strings), but only `--ents` decides what is drawn. No existing command's output
/// depends on this one.
fn cmd_view(args: &[String]) -> Result<Out, String> {
    use std::collections::HashMap;

    let (pak_path, map_name, out) = (&args[0], &args[1], &args[2]);
    let parse_vec3 = |flag: &str, s: &str| -> Result<[f32; 3], String> {
        let v: Vec<f32> = s.split(',').map(|p| p.trim().parse::<f32>()).collect::<Result<_, _>>()
            .map_err(|_| format!("{flag}: expected x,y,z, got {s:?}"))?;
        if v.len() != 3 {
            return Err(format!("{flag}: expected 3 comma-separated numbers, got {s:?}"));
        }
        Ok([v[0], v[1], v[2]])
    };
    let (mut w, mut h) = (320usize, 200usize);
    let (mut origin, mut angles, mut time, mut fov) = (None, None, None, 90.0f32);
    let (mut ents_path, mut viewmodel_arg): (Option<&str>, Option<&str>) = (None, None);
    let mut bench: Option<u32> = None;
    let mut i = 3;
    while i < args.len() {
        let flag = args[i].as_str();
        let val = args.get(i + 1).ok_or_else(|| format!("{flag} needs a value"))?;
        match flag {
            "--res" => {
                let (a, b) = val.split_once(['x', 'X']).ok_or_else(|| format!("--res: expected WxH, got {val:?}"))?;
                w = a.trim().parse().map_err(|_| format!("--res: bad width {a:?}"))?;
                h = b.trim().parse().map_err(|_| format!("--res: bad height {b:?}"))?;
            }
            "--origin" => origin = Some(parse_vec3(flag, val)?),
            "--angles" => angles = Some(parse_vec3(flag, val)?),
            "--time" => time = Some(val.parse::<f32>().map_err(|_| format!("--time: bad number {val:?}"))?),
            "--fov" => fov = val.parse().map_err(|_| format!("--fov: bad number {val:?}"))?,
            "--ents" => ents_path = Some(val.as_str()),
            "--viewmodel" => viewmodel_arg = Some(val.as_str()),
            "--bench" => bench = Some(val.parse::<u32>().map_err(|_| format!("--bench: bad count {val:?}"))?.max(1)),
            other => return Err(format!("view: unknown option {other:?}")),
        }
        i += 2;
    }

    let pak = Pak::open(pak_path).map_err(|e| e.to_string())?;
    let read_pak = |name: &str| -> Result<Vec<u8>, String> {
        pak.read_file(name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("{name} not found in {pak_path}"))
    };
    let bsp_bytes = read_pak(map_name)?;
    let bsp = Bsp::parse(&bsp_bytes).map_err(|e| e.to_string())?;
    let bsp_sim = Bsp::parse(&bsp_bytes).map_err(|e| e.to_string())?;
    let palette = render::parse_palette(&read_pak("gfx/palette.lmp")?)
        .ok_or_else(|| "bad/short gfx/palette.lmp".to_string())?;
    let colormap = pak.read_file("gfx/colormap.lmp").ok().flatten();
    let progs = Progs::parse(&read_pak("progs.dat")?).map_err(|e| e.to_string())?;
    let mut server = Server::with_pak(bsp_sim, progs, Some(pak.clone())).map_err(|e| e.to_string())?;
    server.set_map_name(map_name); // SV_SpawnServer: world.model + the mapname global
    server.spawn_entities().map_err(|e| e.to_string())?;

    // Default camera: the player start at the view height (DEFAULT_VIEWHEIGHT 22).
    let start = player_start(&bsp.entities);
    let origin = match (origin, start) {
        (Some(o), _) => o,
        (None, Some((o, _))) => [o[0], o[1], o[2] + 22.0],
        (None, None) => return Err("map has no info_player_start; pass --origin".into()),
    };
    let angles = angles.unwrap_or([0.0, start.map_or(0.0, |(_, a)| a), 0.0]);
    let cam = Camera { pos: origin, yaw: angles[1], pitch: -angles[0], roll: angles[2], fov_deg: fov };
    let time = time.unwrap_or_else(|| server.time());
    let light_styles = server.lightstyle_scales(time);

    // The entity list, resolved against per-name model caches (each file parsed once).
    let mut mdls: HashMap<String, Option<Mdl>> = HashMap::new();
    let mut sprs: HashMap<String, Option<Sprite>> = HashMap::new();
    let mut ext: HashMap<String, Option<Bsp>> = HashMap::new();
    // (model, origin, angles, frame, skin) per alias entity.
    type AliasDesc = (String, [f32; 3], [f32; 3], usize, i32);
    let mut alias_descs: Vec<AliasDesc> = Vec::new();
    let mut sprite_descs: Vec<(String, [f32; 3], usize)> = Vec::new();
    let mut ext_descs: Vec<(String, [f32; 3])> = Vec::new();
    let mut bmodels: Vec<render::BModelInstance> = Vec::new();
    let mut skipped = 0usize;
    if let Some(p) = ents_path {
        let text = std::fs::read_to_string(p).map_err(|e| format!("cannot read {p}: {e}"))?;
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let f: Vec<&str> = line.split_whitespace().collect();
            let num = |k: usize| f.get(k).and_then(|s| s.parse::<f32>().ok());
            let (Some(model), Some(ox), Some(oy), Some(oz), Some(ap), Some(ay), Some(ar), Some(fr), Some(sk)) =
                (f.first(), num(1), num(2), num(3), num(4), num(5), num(6), num(7), num(8))
            else {
                return Err(format!("{p}: malformed entity line {line:?}"));
            };
            let (model, org, ang) = (model.to_string(), [ox, oy, oz], [ap, ay, ar]);
            if let Some(n) = model.strip_prefix('*') {
                let model_index = n.parse().map_err(|_| format!("{p}: bad submodel {model:?}"))?;
                bmodels.push(render::BModelInstance { model_index, origin: org, frame: fr as i32 });
            } else if model.ends_with(".bsp") {
                ext.entry(model.clone()).or_insert_with(|| read_pak(&model).ok().and_then(|b| Bsp::parse(&b).ok()));
                ext_descs.push((model, org));
            } else if model.ends_with(".spr") {
                sprs.entry(model.clone()).or_insert_with(|| read_pak(&model).ok().and_then(|b| Sprite::parse(&b).ok()));
                sprite_descs.push((model, org, fr as usize));
            } else if model.ends_with(".mdl") {
                mdls.entry(model.clone()).or_insert_with(|| read_pak(&model).ok().and_then(|b| Mdl::parse(&b).ok()));
                alias_descs.push((model, org, ang, fr as usize, sk as i32));
            } else {
                skipped += 1;
            }
        }
    }
    let instances: Vec<render::ModelInstance> = alias_descs
        .iter()
        .filter_map(|(name, org, ang, frame, skin)| match mdls.get(name) {
            Some(Some(mdl)) => Some(render::ModelInstance {
                mdl,
                origin: *org,
                yaw: ang[1],
                pitch: ang[0],
                roll: ang[2],
                color: color_for_name(name),
                frame: *frame,
                skinnum: *skin,
            }),
            _ => None,
        })
        .collect();
    let externals: Vec<render::ExternalBModel> = ext_descs
        .iter()
        .filter_map(|(name, org)| match ext.get(name) {
            Some(Some(bsp)) => Some(render::ExternalBModel { bsp, origin: *org }),
            _ => None,
        })
        .collect();
    let sprites: Vec<render::SpriteInstance> = sprite_descs
        .iter()
        .filter_map(|(name, org, frame)| match sprs.get(name) {
            Some(Some(sprite)) => Some(render::SpriteInstance { sprite, origin: *org, frame: *frame }),
            _ => None,
        })
        .collect();
    let vm_mdl = match viewmodel_arg {
        Some(arg) => {
            let (name, frame) = arg.rsplit_once(':').unwrap_or((arg, "0"));
            let frame: usize = frame.parse().map_err(|_| format!("--viewmodel: bad frame in {arg:?}"))?;
            Some((Mdl::parse(&read_pak(name)?).map_err(|e| e.to_string())?, frame))
        }
        None => None,
    };
    let unresolved = alias_descs.len() + ext_descs.len() + sprite_descs.len()
        - instances.len() - externals.len() - sprites.len();

    let render_once = || {
        let viewmodel = vm_mdl.as_ref().map(|(mdl, frame)| render::Viewmodel {
            mdl,
            frame: *frame,
            // The view is full-frame (id at viewsize 120, the oracle default): no fudge, no bob.
            origin_ofs: render::viewmodel_origin_ofs(&cam, 0.0, 120.0),
        });
        render::render_scene_ext_sprited(
            &bsp, &cam, w, h, &palette, &instances, &bmodels, &externals, viewmodel, time, &[], &[],
            &light_styles, colormap.as_deref(), &sprites,
        )
    };
    let img = render_once();
    // Warm re-renders of the same view (the first, cold frame above is excluded),
    // the port side of the oracle's `oracle_bench`: renderer cost only.
    let bench = bench.map(|n| {
        let start = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box(render_once());
        }
        (n, start.elapsed().as_secs_f64() * 1000.0 / n as f64)
    });
    img.write_ppm(out).map_err(|e| format!("cannot write {out}: {e}"))?;
    let mut o = String::new();
    let _ = writeln!(
        o,
        "view {map_name} {w}x{h} origin [{} {} {}] angles [{} {} {}] fov {fov} time {time}",
        origin[0], origin[1], origin[2], angles[0], angles[1], angles[2]
    );
    let _ = writeln!(
        o,
        "  entities: {} alias, {} submodel, {} external, {} sprite ({} unresolved, {} unknown kind)",
        instances.len(), bmodels.len(), externals.len(), sprites.len(), unresolved, skipped
    );
    if let Some((n, per)) = bench {
        let _ = writeln!(o, "  bench {n} warm frames -> {per:.4} ms/frame ({:.1} fps)", 1000.0 / per);
    }
    let _ = writeln!(o, "  -> {out} ({}x{} PPM)", img.w, img.h);
    Ok(Out::Text(o))
}

/// `walk <pak> <map.bsp> <out-prefix> [steps]` — spawn the map, then walk a
/// player box forward from `info_player_start` along its facing angle, writing
/// one rendered PPM frame per step (`<prefix>_000.ppm`, …). Movement uses the
/// world slide-move ([`quake_rs::world::walk_move`]) so the player follows walls
/// and stops at them; the spawned `.mdl` entities are drawn into every frame.
fn cmd_walk(pak_path: &str, map_name: &str, out_prefix: &str, steps: u32) -> Result<Out, String> {
    let pak = Pak::open(pak_path).map_err(|e| e.to_string())?;
    let read_pak = |name: &str| -> Result<Vec<u8>, String> {
        pak.read_file(name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("{name} not found in {pak_path}"))
    };
    let bsp_bytes = read_pak(map_name)?;
    let bsp = Bsp::parse(&bsp_bytes).map_err(|e| e.to_string())?; // render + collision
    let bsp_sim = Bsp::parse(&bsp_bytes).map_err(|e| e.to_string())?; // moved into the server
    let palette = render::parse_palette(&read_pak("gfx/palette.lmp")?)
        .ok_or_else(|| "bad/short gfx/palette.lmp".to_string())?;
    let progs = Progs::parse(&read_pak("progs.dat")?).map_err(|e| e.to_string())?;

    let (spawn, ang) =
        player_start(&bsp.entities).ok_or_else(|| "map has no info_player_start".to_string())?;

    // Spawn entities and gather their MDL models (drawn at fixed positions).
    let mut server = Server::with_pak(bsp_sim, progs, Some(pak.clone())).map_err(|e| e.to_string())?;
    server.set_map_name(map_name); // SV_SpawnServer: world.model + the mapname global
    server.spawn_entities().map_err(|e| e.to_string())?;
    let mut model_cache: std::collections::HashMap<String, Option<Mdl>> =
        std::collections::HashMap::new();
    let mut owned: Vec<(Mdl, [f32; 3], f32, [u8; 3])> = Vec::new();
    for e in 0..server.vm.num_edicts() {
        if server.vm.edict_free.get(e).copied().unwrap_or(true) {
            continue;
        }
        let ent = e as i32;
        let model = server.vm.ent_get_string(ent, "model");
        if model.is_empty()
            || model.starts_with('*')
            || model.starts_with("maps/")
            || !model.ends_with(".mdl")
        {
            continue;
        }
        if !model_cache.contains_key(&model) {
            let parsed = match pak.read_file(&model) {
                Ok(Some(b)) => Mdl::parse(&b).ok(),
                _ => None,
            };
            model_cache.insert(model.clone(), parsed);
        }
        if let Some(Some(mdl)) = model_cache.get(&model) {
            let origin = server.vm.ent_get_vector(ent, "origin");
            let yaw = server.vm.ent_get_vector(ent, "angles")[1];
            owned.push((mdl.clone(), origin, yaw, color_for_name(&model)));
        }
    }
    let instances: Vec<render::ModelInstance> = owned
        .iter()
        .map(|(mdl, origin, yaw, color)| render::ModelInstance {
            mdl,
            origin: *origin,
            yaw: *yaw, pitch: 0.0, roll: 0.0,
            color: *color,
            frame: 0,
            skinnum: 0,
        })
        .collect();

    // Walk forward from the spawn along its facing yaw.
    let (mins, maxs) = ([-16.0f32, -16.0, -24.0], [16.0f32, 16.0, 32.0]);
    let yr = ang.to_radians();
    let forward = [yr.cos(), yr.sin(), 0.0f32];
    let (w, h) = (480usize, 300usize);
    let (dt, speed) = (0.1f32, 180.0f32);

    // Settle onto the floor before the first step.
    let mut origin = quake_rs::world::walk_move(&bsp, spawn, mins, maxs, [0.0, 0.0, 0.0], dt);
    let start = origin;
    let mut frames = 0u32;
    for i in 0..steps {
        let wishvel = [forward[0] * speed, forward[1] * speed, 0.0];
        origin = quake_rs::world::walk_move(&bsp, origin, mins, maxs, wishvel, dt);
        let eye = [origin[0], origin[1], origin[2] + 22.0];
        let cam = Camera::looking_at(eye, [eye[0] + forward[0], eye[1] + forward[1], eye[2]], 90.0);
        let img = render::render_scene(&bsp, &cam, w, h, &palette, &instances);
        let path = format!("{out_prefix}_{i:03}.ppm");
        img.write_ppm(&path).map_err(|e| format!("cannot write {path}: {e}"))?;
        frames += 1;
    }

    let dist = {
        let (dx, dy) = (origin[0] - start[0], origin[1] - start[1]);
        (dx * dx + dy * dy).sqrt()
    };
    let mut o = String::new();
    let _ = writeln!(
        o,
        "walk {map_name}: {} model instances, {frames} frames @ {w}x{h}",
        instances.len()
    );
    let _ = writeln!(
        o,
        "  spawn {start:?} -> end {origin:?}  (advanced {dist:.0} units; the slide stops at walls)"
    );
    let _ = writeln!(o, "  wrote {out_prefix}_000.ppm .. {out_prefix}_{:03}.ppm", frames.saturating_sub(1));
    Ok(Out::Text(o))
}

/// `demo <pak> <demo.dem> <out-prefix> [stride]` — replay a recorded Quake demo
/// (id's attract-mode `.dem`) and render it: parse the net-protocol stream into
/// per-frame entity snapshots, then draw the map + each entity's `.mdl` from the
/// recorded viewpoint, one PPM per sampled server frame. `stride` 0 = auto-pick
/// to emit ~120 frames.
fn cmd_demo(pak_path: &str, demo_name: &str, out_prefix: &str, stride_arg: usize) -> Result<Out, String> {
    let pak = Pak::open(pak_path).map_err(|e| e.to_string())?;
    let read_pak = |name: &str| -> Result<Vec<u8>, String> {
        pak.read_file(name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("{name} not found in {pak_path}"))
    };

    let demo = quake_rs::demo::parse_demo(&read_pak(demo_name)?).map_err(|e| e.to_string())?;
    let map = demo
        .map_name()
        .ok_or_else(|| "demo has no world model (never received serverinfo)".to_string())?
        .to_string();
    let bsp = Bsp::parse(&read_pak(&map)?).map_err(|e| e.to_string())?;
    let palette = render::parse_palette(&read_pak("gfx/palette.lmp")?)
        .ok_or_else(|| "bad/short gfx/palette.lmp".to_string())?;

    let total = demo.frames.len();
    let stride = if stride_arg == 0 { (total / 120).max(1) } else { stride_arg };
    let (w, h) = (480usize, 300usize);

    let mut model_cache: std::collections::HashMap<String, Option<Mdl>> =
        std::collections::HashMap::new();
    let mut written = 0u32;
    for f in demo.frames.iter().step_by(stride) {
        // Build the alias-model instances visible this frame.
        let mut owned: Vec<(Mdl, [f32; 3], f32, [u8; 3])> = Vec::new();
        for e in &f.entities {
            let name = match demo.model_precache.get(e.modelindex) {
                Some(n) if n.ends_with(".mdl") => n.clone(),
                _ => continue,
            };
            if !model_cache.contains_key(&name) {
                let parsed = match pak.read_file(&name) {
                    Ok(Some(b)) => Mdl::parse(&b).ok(),
                    _ => None,
                };
                model_cache.insert(name.clone(), parsed);
            }
            if let Some(Some(mdl)) = model_cache.get(&name) {
                owned.push((mdl.clone(), e.origin, e.angles[1], color_for_name(&name)));
            }
        }
        let instances: Vec<render::ModelInstance> = owned
            .iter()
            .map(|(mdl, origin, yaw, color)| render::ModelInstance {
                mdl,
                origin: *origin,
                yaw: *yaw, pitch: 0.0, roll: 0.0,
                color: *color,
                frame: 0,
                skinnum: 0,
            })
            .collect();

        // Demo view angles are [pitch, yaw, roll]; the renderer's pitch is +up,
        // while Quake's is +down, so negate it. view_origin already includes the
        // view height. Build the camera directly from the recorded angles.
        let cam = Camera {
            pos: f.view_origin,
            yaw: f.view_angles[1],
            pitch: -f.view_angles[0],
            // Demos record viewangles[ROLL] (the engine's V_CalcViewRoll bank); use
            // it so demo playback leans/tilts exactly as the original did.
            roll: f.view_angles[2],
            fov_deg: 90.0,
        };
        let img = render::render_scene(&bsp, &cam, w, h, &palette, &instances);
        let path = format!("{out_prefix}_{written:04}.ppm");
        img.write_ppm(&path).map_err(|e| format!("cannot write {path}: {e}"))?;
        written += 1;
    }

    let mut o = String::new();
    let _ = writeln!(o, "demo {demo_name}: map {map}, {total} server frames", );
    let _ = writeln!(
        o,
        "  {} models precached, {} unique .mdl loaded; rendered {written} frames (stride {stride}) @ {w}x{h}",
        demo.model_precache.len().saturating_sub(1),
        model_cache.values().filter(|v| v.is_some()).count()
    );
    let _ = writeln!(o, "  wrote {out_prefix}_0000.ppm .. {out_prefix}_{:04}.ppm", written.saturating_sub(1));
    Ok(Out::Text(o))
}
