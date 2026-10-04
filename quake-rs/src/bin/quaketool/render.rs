//! The render commands: the port's software renderer (`quake_rs::render`)
//! drawing into a PPM.
//!
//! - `render`, `render-demo` and `menu` (here): a BSP seen from its start,
//!   the built-in test room, and the MAIN menu over e1m1;
//! - `scene` ([`scene`]): a map with its spawned models — the golden renders;
//! - `view` ([`view`]): one exactly specified view, the port's side of the C
//!   oracle's pixel diff;
//! - `shot` ([`shot`]): the finished game screen as a player sees it, at any
//!   size and video setting.

use std::fmt::Write as _;

use quake_rs::bsp::Bsp;
use quake_rs::pak::Pak;
use quake_rs::render::{self, Camera};
use quake_rs::wad::Wad2;

use crate::entities::player_start;
use crate::{read, CmdResult, Out};

pub mod scene;
pub mod shot;
pub mod view;

/// Camera at the player spawn (eye height +24), facing the spawn angle; falls
/// back to the centre of the map bounds looking +X if there's no spawn.
pub fn camera_for_bsp(b: &Bsp) -> Camera {
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

pub fn cmd_render(path: &str, out: &str, palette: Option<&str>) -> CmdResult {
    let bytes = read(path)?;
    let b = Bsp::parse(&bytes)?;
    let cam = camera_for_bsp(&b);
    let (img, mode) = match palette {
        Some(pp) => {
            let pbytes = read(pp)?;
            let pal = render::parse_palette(&pbytes)
                .ok_or_else(|| format!("bad palette {pp} (need >= 768 bytes)"))?;
            (render::Renderer::new().render(&render::Scene::new(&b, cam, 640, 400, &pal)).to_rgb(&pal), "textured")
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

pub fn cmd_render_demo(out: &str) -> CmdResult {
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
pub fn cmd_menu(pak_path: &str, out: &str) -> CmdResult {
    let pak = Pak::open(pak_path)?;
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
            (render::Renderer::new().render(&render::Scene::new(&b, cam, W, H, &palette)), "e1m1 POV")
        }
        None => (render::Image::new(W, H, 0), "black frame"),
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
        textbox: std::array::from_fn(|i| lmp(quake_rs::menu::TEXTBOX_PICS[i])),
        bigbox: lmp("gfx/bigbox.lmp"),
        menuplyr: lmp("gfx/menuplyr.lmp"),
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
    let settings = quake_rs::settings::Settings::new(quake_rs::settings::Preset::Classic, quake_rs::settings::Machine::default());
    render::draw_menu(&mut img, &menu, &settings, &pics, conchars.as_ref(), render::MenuClock::default());

    img.to_rgb(&palette).write_ppm(out).map_err(|e| format!("cannot write {out}: {e}"))?;

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
pub fn color_for_name(name: &str) -> [u8; 3] {
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
