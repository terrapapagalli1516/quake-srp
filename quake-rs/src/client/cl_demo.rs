//! cl_demo.c — playing a recorded `.dem`: `CL_PlayDemo_f`'s build of a
//! [`DemoPlay`] and quake.rc's demo loop.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/cl_demo.c`.

use crate::bsp::Bsp;
use crate::demo::parse_demo;
use crate::mdl::Mdl;
use crate::pak::Pak;
use crate::render;
use crate::wad::Qpic;

use super::{color_for_name, DemoPlay, SoundCall};

/// quake.rc's `startdemos demo1 demo2 demo3`: the attract loop's demos, played
/// in turn (`CL_NextDemo` on each demo's `svc_disconnect`), wrapping to the first.
pub const DEMOS: [&str; 3] = ["demo1.dem", "demo2.dem", "demo3.dem"];

/// `playdemo` of [`DEMOS`]`[demonum % 3]` from `pak` (`CL_PlayDemo_f`): the
/// demo's sounds start through `sound` ([`SoundCall::StopAll`], then the
/// signon's static loops).
pub fn build_demo_n(pak: Pak, demonum: usize, sound: &mut Vec<SoundCall>) -> Option<DemoPlay> {
    let demonum = demonum % DEMOS.len();
    let read = |n: &str| pak.read_file(n).ok().flatten();
    let demo_bytes = read(DEMOS[demonum])?;
    let demo = parse_demo(&demo_bytes).ok()?;
    let map = demo.map_name()?.to_string();
    let bsp = Bsp::parse(&read(&map)?).ok()?;
    let palette = render::parse_palette(&read("gfx/palette.lmp")?)?;

    // Load a model (alias or sprite) + colour per precache index.
    let mut models = Vec::with_capacity(demo.model_precache.len());
    let mut sprites: Vec<Option<crate::spr::Sprite>> =
        Vec::with_capacity(demo.model_precache.len());
    let mut colors = Vec::with_capacity(demo.model_precache.len());
    for name in &demo.model_precache {
        if name.ends_with(".mdl") {
            models.push(read(name).and_then(|b| Mdl::parse(&b).ok()));
            sprites.push(None);
        } else if name.ends_with(".spr") {
            models.push(None);
            sprites.push(read(name).and_then(|b| crate::spr::Sprite::parse(&b).ok()));
        } else {
            models.push(None);
            sprites.push(None);
        }
        colors.push(color_for_name(name));
    }
    if demo.frames.is_empty() {
        return None;
    }
    // Re-parse with inter-frame interpolation — smooth 60 fps playback instead of
    // the choppy 10 Hz keyframes (CL_LerpPoint), plus EF_ROTATE spin for any model
    // whose header flags it (rotating pickups). The set of rotating model indices
    // is derived from the just-loaded MDL headers.
    let rotating: Vec<usize> = models
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            m.as_ref()
                .filter(|md| md.header.flags & crate::demo::EF_ROTATE != 0)
                .map(|_| i)
        })
        .collect();
    let demo = crate::demo::parse_demo_interpolated(&demo_bytes, 60.0, &rotating).ok()?;
    if demo.frames.is_empty() {
        return None;
    }
    // Demo committed (nothing below fails): tear down the previous level/mode's
    // looping audio and register the demo signon's `svc_spawnstaticsound` loops
    // (CL_ParseStaticSound ran these on the live client during demo playback
    // too — the e1m3 demo has its own torches).
    sound.push(SoundCall::StopAll);
    sound.push(SoundCall::Static(demo.static_sounds.clone()));
    // The overlay assets for a recorded intermission/finale (each optional —
    // a demo without one never touches them).
    let gfx_wad = read("gfx.wad").and_then(|b| crate::wad::Wad2::parse(b).ok());
    let conchars = gfx_wad.as_ref().and_then(render::conchars_pic);
    let lmp = |n: &str| -> Option<Qpic> { read(n).and_then(|b| Qpic::parse(&b).ok()) };
    // Resolve everything that reads the pak BEFORE the struct literal so the
    // `read`/`lmp` closure borrows end and `pak` can move into the DemoPlay.
    let colormap = read("gfx/colormap.lmp");
    let pic_complete = lmp("gfx/complete.lmp");
    let pic_inter = lmp("gfx/inter.lmp");
    let pic_finale = lmp("gfx/finale.lmp");
    let mut d = DemoPlay::new(pak, bsp, palette, demo);
    d.models = models;
    d.sprites = sprites;
    d.colormap = colormap;
    d.colors = colors;
    d.gfx_wad = gfx_wad;
    d.conchars = conchars;
    d.pic_complete = pic_complete;
    d.pic_inter = pic_inter;
    d.pic_finale = pic_finale;
    d.demonum = demonum;
    Some(d)
}
