//! End-to-end tests that drive `quake_rs` the way a real user (and the engine)
//! would: assets packed into a PAK, read back from disk, then parsed by the
//! format loaders. These complement the in-module unit tests, which work purely
//! in memory.

use quake_rs::bsp::Bsp;
use quake_rs::crc;
use quake_rs::pak::Pak;
use quake_rs::wad::{Wad2, TYP_PALETTE};

// ---------------------------------------------------------------------------
// Synthetic file builders (little-endian, matching the on-disk layouts)
// ---------------------------------------------------------------------------

/// Build a PAK image from `(name, data)` pairs. Layout: 12-byte header, then
/// concatenated file data, then the directory of 64-byte `dpackfile_t` entries.
fn build_pak(files: &[(&str, &[u8])]) -> Vec<u8> {
    const HEADER: usize = 12;
    let mut body = Vec::new();
    let mut offsets = Vec::new();
    for (_, data) in files {
        offsets.push(HEADER + body.len());
        body.extend_from_slice(data);
    }
    let mut dir = Vec::new();
    for (i, (name, data)) in files.iter().enumerate() {
        let mut name_field = [0u8; 56];
        let nb = name.as_bytes();
        let n = nb.len().min(56);
        name_field[..n].copy_from_slice(&nb[..n]);
        dir.extend_from_slice(&name_field);
        dir.extend_from_slice(&(offsets[i] as i32).to_le_bytes());
        dir.extend_from_slice(&(data.len() as i32).to_le_bytes());
    }
    let dirofs = HEADER + body.len();
    let mut out = Vec::new();
    out.extend_from_slice(b"PACK");
    out.extend_from_slice(&(dirofs as i32).to_le_bytes());
    out.extend_from_slice(&(dir.len() as i32).to_le_bytes());
    out.extend_from_slice(&body);
    out.extend_from_slice(&dir);
    out
}

/// Build a minimal BSP v29 with the given vertices and entity text. Only the
/// ENTITIES (lump 0) and VERTEXES (lump 3) lumps carry data; the rest are empty.
fn build_bsp(vertices: &[[f32; 3]], entities: &str) -> Vec<u8> {
    const HEADER: usize = 124; // version(4) + 15 * lump(8)
    let mut ent = entities.as_bytes().to_vec();
    ent.push(0); // NUL-terminated, like the C writer
    let mut verts = Vec::new();
    for v in vertices {
        for c in v {
            verts.extend_from_slice(&c.to_le_bytes());
        }
    }
    let ent_ofs = HEADER;
    let vert_ofs = ent_ofs + ent.len();

    let mut lumps = [(0i32, 0i32); 15];
    lumps[0] = (ent_ofs as i32, ent.len() as i32); // LUMP_ENTITIES
    lumps[3] = (vert_ofs as i32, verts.len() as i32); // LUMP_VERTEXES

    let mut out = Vec::new();
    out.extend_from_slice(&29i32.to_le_bytes());
    for (o, l) in lumps {
        out.extend_from_slice(&o.to_le_bytes());
        out.extend_from_slice(&l.to_le_bytes());
    }
    out.extend_from_slice(&ent);
    out.extend_from_slice(&verts);
    out
}

/// Build a WAD2 with a single 768-byte palette lump.
fn build_wad_with_palette() -> Vec<u8> {
    let palette: Vec<u8> = (0..768).map(|i| (i % 256) as u8).collect();
    const HEADER: usize = 12;
    const LUMPINFO: usize = 32;
    let infotableofs = HEADER + palette.len();
    let mut out = Vec::new();
    out.extend_from_slice(b"WAD2");
    out.extend_from_slice(&1i32.to_le_bytes()); // numlumps
    out.extend_from_slice(&(infotableofs as i32).to_le_bytes());
    out.extend_from_slice(&palette);
    // one lumpinfo
    out.extend_from_slice(&(HEADER as i32).to_le_bytes()); // filepos
    out.extend_from_slice(&(palette.len() as i32).to_le_bytes()); // disksize
    out.extend_from_slice(&(palette.len() as i32).to_le_bytes()); // size
    out.push(TYP_PALETTE); // type
    out.push(0); // compression
    out.push(0); // pad1
    out.push(0); // pad2
    let mut name = [0u8; 16];
    name[..7].copy_from_slice(b"PALETTE");
    out.extend_from_slice(&name);
    let _ = LUMPINFO;
    out
}

fn temp_path(tag: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("quakers_it_{}_{}.bin", std::process::id(), tag));
    p
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn pak_open_from_disk_round_trips() {
    let hello = b"hello, quake";
    let world = b"another file's bytes";
    let img = build_pak(&[("progs/hello.txt", hello), ("maps/world.dat", world)]);

    let path = temp_path("pak_disk");
    std::fs::write(&path, &img).unwrap();

    let pak = Pak::open(&path).expect("open pak");
    assert_eq!(pak.entries().len(), 2);
    assert!(pak.find("progs/hello.txt").is_some());
    assert!(pak.find("does/not/exist").is_none());

    // read_file goes through the File source: open + seek + read.
    assert_eq!(pak.read_file("progs/hello.txt").unwrap().unwrap(), hello);
    assert_eq!(pak.read_file("maps/world.dat").unwrap().unwrap(), world);
    assert!(pak.read_file("missing").unwrap().is_none());

    std::fs::remove_file(&path).ok();
}

#[test]
fn pak_directory_crc_matches_independent_computation() {
    let img = build_pak(&[("a", b"aaa"), ("bb", b"bbbb"), ("ccc", b"ccccc")]);
    let pak = Pak::from_bytes("test.pak".into(), img.clone()).unwrap();

    // Independently CRC the directory region the way COM_LoadPackFile does.
    let dirofs = i32::from_le_bytes([img[4], img[5], img[6], img[7]]) as usize;
    let dirlen = i32::from_le_bytes([img[8], img[9], img[10], img[11]]) as usize;
    let expect = crc::crc_block(&img[dirofs..dirofs + dirlen]);

    assert_eq!(pak.dir_crc(), expect);
    // A synthetic pak is never the stock shareware pak0.pak.
    assert!(pak.is_modified());
}

#[test]
fn bsp_parses_and_reports_geometry() {
    let verts = [
        [0.0, 0.0, 0.0],
        [64.0, 0.0, 0.0],
        [64.0, 128.0, 0.0],
        [0.0, 128.0, 16.0],
    ];
    let ents = "{\n\"classname\" \"worldspawn\"\n\"wad\" \"gfx.wad\"\n}\n\
                {\n\"classname\" \"info_player_start\"\n\"origin\" \"32 64 24\"\n}";
    let img = build_bsp(&verts, ents);

    let b = Bsp::parse(&img).expect("parse bsp");
    assert_eq!(b.version, 29);
    assert_eq!(b.vertexes.len(), 4);
    assert_eq!(b.vertexes[1].point, [64.0, 0.0, 0.0]);
    assert!(b.entities.contains("info_player_start"));
    // Bounds the `map` command would use.
    let max_y = b.vertexes.iter().fold(f32::MIN, |m, v| m.max(v.point[1]));
    assert_eq!(max_y, 128.0);
}

#[test]
fn assets_packed_in_a_pak_load_end_to_end() {
    // This mirrors how Quake actually loads data: maps and models live inside a
    // PAK, are pulled out by name, then parsed by the format loaders.
    let bsp_bytes = build_bsp(&[[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]], "{ \"classname\" \"worldspawn\" }");
    let wad_bytes = build_wad_with_palette();
    let img = build_pak(&[
        ("maps/start.bsp", &bsp_bytes),
        ("gfx.wad", &wad_bytes),
    ]);

    let path = temp_path("pak_assets");
    std::fs::write(&path, &img).unwrap();
    let pak = Pak::open(&path).unwrap();

    // Pull the map out of the pak and parse it.
    let raw_map = pak.read_file("maps/start.bsp").unwrap().unwrap();
    let map = Bsp::parse(&raw_map).unwrap();
    assert_eq!(map.version, 29);
    assert_eq!(map.vertexes.len(), 2);

    // Pull the WAD out and parse it.
    let raw_wad = pak.read_file("gfx.wad").unwrap().unwrap();
    let wad = Wad2::parse(raw_wad).unwrap();
    assert_eq!(wad.lumps().len(), 1);
    let pal = wad.lump("palette").expect("palette lump");
    assert_eq!(pal.typ, TYP_PALETTE);
    assert_eq!(wad.lump_data(pal).unwrap().len(), 768);

    std::fs::remove_file(&path).ok();
}

#[test]
fn wad_round_trips_from_outside_the_crate() {
    let wad = Wad2::parse(build_wad_with_palette()).unwrap();
    assert_eq!(wad.lumps().len(), 1);
    // cleanup_name lowercases — the on-disk name was "PALETTE".
    assert_eq!(wad.lumps()[0].name, "palette");
    assert!(wad.lump("PALETTE").is_some(), "lookup is case-insensitive");
}
