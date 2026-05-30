//! Browser (WASM) shell for `quake-rs`: an interactive first-person walk *and*
//! recorded-demo playback, rendered to an RGBA framebuffer the page blits to a
//! canvas.
//!
//! The engine crate stays `#![forbid(unsafe_code)]` and compiles to
//! `wasm32-unknown-unknown` unchanged. This shell has **zero `unsafe {}` blocks**
//! — it just can't `forbid(unsafe_code)` because modern Rust treats the
//! `#[no_mangle]` export attribute as unsafe-adjacent. The pak is embedded with
//! `include_bytes!` so nothing crosses JS→WASM (no `from_raw_parts`); state lives
//! in a `thread_local`; the page reads the framebuffer out of linear memory via
//! the `Vec::as_ptr()` we hand back. No `wasm-bindgen`, no dependencies.

use std::cell::RefCell;

use quake_rs::bsp::Bsp;
use quake_rs::demo::{parse_demo, Demo};
use quake_rs::mdl::Mdl;
use quake_rs::progs::Progs;
use quake_rs::render::{self, Camera, ModelInstance};
use quake_rs::server::Server;

static PAK: &[u8] = include_bytes!("../../quake-data/ID1/PAK0.PAK");

const WALK_MAP: &str = "maps/e1m1.bsp";
const DEMO_FILE: &str = "demo1.dem";
const W: usize = 320;
const H: usize = 200;
const SPEED: f32 = 320.0;
const PLAYER_MINS: [f32; 3] = [-16.0, -16.0, -24.0];
const PLAYER_MAXS: [f32; 3] = [16.0, 16.0, 32.0];

/// Interactive walk state.
struct Walk {
    bsp: Bsp,
    palette: [[u8; 3]; 256],
    models: Vec<(Mdl, [f32; 3], f32, [u8; 3])>,
    origin: [f32; 3],
    yaw: f32,
    pitch: f32,
    in_fwd: f32,
    in_side: f32,
}

/// Recorded-demo playback state.
struct DemoPlay {
    bsp: Bsp,
    palette: [[u8; 3]; 256],
    demo: Demo,
    /// Parsed model per precache index (None for non-`.mdl` / missing).
    models: Vec<Option<Mdl>>,
    colors: Vec<[u8; 3]>,
    elapsed: f32,
    idx: usize,
}

struct App {
    walk: Option<Walk>,
    demo: Option<DemoPlay>,
    /// 0 = walk, 1 = demo.
    mode: u8,
    fb: Vec<u8>, // RGBA, W*H*4
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn color_for_name(name: &str) -> [u8; 3] {
    let mut h: u32 = 2166136261;
    for b in name.bytes() {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    let table = [
        [220, 80, 80], [80, 200, 120], [90, 130, 230], [220, 200, 90],
        [200, 110, 210], [110, 210, 210], [230, 150, 80],
    ];
    table[(h % table.len() as u32) as usize]
}

fn player_start(ents: &str) -> Option<([f32; 3], f32)> {
    for block in ents.split('}') {
        let toks: Vec<&str> = block.split('"').collect();
        let (mut classname, mut origin, mut angle) = ("", None, 0.0f32);
        let mut i = 1;
        while i + 2 < toks.len() {
            match toks[i] {
                "classname" => classname = toks[i + 2],
                "origin" => {
                    let n: Vec<f32> =
                        toks[i + 2].split_whitespace().filter_map(|s| s.parse().ok()).collect();
                    if n.len() == 3 {
                        origin = Some([n[0], n[1], n[2]]);
                    }
                }
                "angle" => angle = toks[i + 2].trim().parse().unwrap_or(0.0),
                _ => {}
            }
            i += 4;
        }
        if classname == "info_player_start" {
            if let Some(o) = origin {
                return Some((o, angle));
            }
        }
    }
    None
}

fn pak() -> Option<quake_rs::pak::Pak> {
    quake_rs::pak::Pak::from_bytes("pak0.pak".into(), PAK.to_vec()).ok()
}

fn build_walk() -> Option<Walk> {
    let pak = pak()?;
    let read = |n: &str| pak.read_file(n).ok().flatten();
    let bsp = Bsp::parse(&read(WALK_MAP)?).ok()?;
    let bsp_sim = Bsp::parse(&read(WALK_MAP)?).ok()?;
    let palette = render::parse_palette(&read("gfx/palette.lmp")?)?;
    let progs = Progs::parse(&read("progs.dat")?).ok()?;
    let (spawn, yaw) = player_start(&bsp.entities).unwrap_or(([0.0, 0.0, 0.0], 0.0));

    let mut server = Server::new(bsp_sim, progs).ok()?;
    let _ = server.spawn_entities();
    let mut cache: std::collections::HashMap<String, Option<Mdl>> = std::collections::HashMap::new();
    let mut models = Vec::new();
    for e in 0..server.vm.num_edicts() {
        if server.vm.edict_free.get(e).copied().unwrap_or(true) {
            continue;
        }
        let ent = e as i32;
        let m = server.vm.ent_get_string(ent, "model");
        if !m.ends_with(".mdl") {
            continue;
        }
        if !cache.contains_key(&m) {
            cache.insert(m.clone(), read(&m).and_then(|b| Mdl::parse(&b).ok()));
        }
        if let Some(Some(mdl)) = cache.get(&m) {
            let origin = server.vm.ent_get_vector(ent, "origin");
            let ya = server.vm.ent_get_vector(ent, "angles")[1];
            models.push((mdl.clone(), origin, ya, color_for_name(&m)));
        }
    }
    Some(Walk { bsp, palette, models, origin: spawn, yaw, pitch: 0.0, in_fwd: 0.0, in_side: 0.0 })
}

fn build_demo() -> Option<DemoPlay> {
    let pak = pak()?;
    let read = |n: &str| pak.read_file(n).ok().flatten();
    let demo = parse_demo(&read(DEMO_FILE)?).ok()?;
    let map = demo.map_name()?.to_string();
    let bsp = Bsp::parse(&read(&map)?).ok()?;
    let palette = render::parse_palette(&read("gfx/palette.lmp")?)?;

    // Load a model + colour per precache index.
    let mut models = Vec::with_capacity(demo.model_precache.len());
    let mut colors = Vec::with_capacity(demo.model_precache.len());
    for name in &demo.model_precache {
        if name.ends_with(".mdl") {
            models.push(read(name).and_then(|b| Mdl::parse(&b).ok()));
        } else {
            models.push(None);
        }
        colors.push(color_for_name(name));
    }
    if demo.frames.is_empty() {
        return None;
    }
    Some(DemoPlay { bsp, palette, demo, models, colors, elapsed: 0.0, idx: 0 })
}

fn ensure_app(f: impl FnOnce(&mut App)) {
    APP.with(|c| {
        if c.borrow().is_none() {
            *c.borrow_mut() = Some(App { walk: None, demo: None, mode: 0, fb: vec![0u8; W * H * 4] });
        }
        if let Some(a) = c.borrow_mut().as_mut() {
            f(a);
        }
    });
}

// ---------------------------------------------------------------------------
// Exports
// ---------------------------------------------------------------------------

/// Start interactive walk mode (e1m1). Returns 1 on success.
#[no_mangle]
pub extern "C" fn boot() -> i32 {
    let w = build_walk();
    let ok = w.is_some();
    ensure_app(|a| {
        a.walk = w;
        a.mode = 0;
    });
    ok as i32
}

/// Start recorded-demo playback (demo1.dem / e1m3). Returns 1 on success.
#[no_mangle]
pub extern "C" fn boot_demo() -> i32 {
    let d = build_demo();
    let ok = d.is_some();
    ensure_app(|a| {
        a.demo = d;
        if a.demo.is_some() {
            a.mode = 1;
        }
    });
    ok as i32
}

#[no_mangle]
pub extern "C" fn width() -> i32 {
    W as i32
}
#[no_mangle]
pub extern "C" fn height() -> i32 {
    H as i32
}

#[no_mangle]
pub extern "C" fn set_move(fwd: f32, side: f32) {
    ensure_app(|a| {
        if let Some(w) = a.walk.as_mut() {
            w.in_fwd = fwd;
            w.in_side = side;
        }
    });
}

#[no_mangle]
pub extern "C" fn look(dyaw: f32, dpitch: f32) {
    ensure_app(|a| {
        if let Some(w) = a.walk.as_mut() {
            w.yaw += dyaw;
            w.pitch = (w.pitch + dpitch).clamp(-70.0, 70.0);
        }
    });
}

/// Advance the active mode by `dt` seconds and render into the framebuffer.
#[no_mangle]
pub extern "C" fn step(dt: f32) {
    ensure_app(|a| {
        let img = if a.mode == 1 {
            a.demo.as_mut().map(|d| step_demo(d, dt))
        } else {
            a.walk.as_mut().map(|w| step_walk(w, dt))
        };
        if let Some(img) = img {
            let fb = &mut a.fb;
            fb.clear();
            for px in &img.rgb {
                fb.push(px[0]);
                fb.push(px[1]);
                fb.push(px[2]);
                fb.push(255);
            }
        }
    });
}

#[no_mangle]
pub extern "C" fn framebuffer() -> *const u8 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| a.fb.as_ptr())
            .unwrap_or(std::ptr::null())
    })
}

// --- sound: hand a real Quake .wav out of the pak for the page to play -------

thread_local! {
    static SND: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Load a recognisable Quake SFX (item pickup) from the pak into a buffer once,
/// returning its byte length. The bytes are a standard RIFF/WAV the browser's
/// `decodeAudioData` understands — this is real id sound data, read by our pak
/// loader, played in the page via Web Audio.
#[no_mangle]
pub extern "C" fn load_sound() -> i32 {
    SND.with(|s| {
        if s.borrow().is_empty() {
            if let Some(p) = pak() {
                if let Ok(Some(b)) = p.read_file("sound/items/r_item1.wav") {
                    *s.borrow_mut() = b;
                }
            }
        }
        s.borrow().len() as i32
    })
}

/// Pointer to the loaded sound bytes in linear memory (the page reads them out).
#[no_mangle]
pub extern "C" fn sound_ptr() -> *const u8 {
    SND.with(|s| s.borrow().as_ptr())
}

// ---------------------------------------------------------------------------
// Per-mode rendering
// ---------------------------------------------------------------------------

fn step_walk(w: &mut Walk, dt: f32) -> render::Image {
    let yr = w.yaw.to_radians();
    let (cy, sy) = (yr.cos(), yr.sin());
    let (mut fwd, mut side) = (w.in_fwd, w.in_side);
    let mag = (fwd * fwd + side * side).sqrt();
    if mag > 1.0 {
        fwd /= mag;
        side /= mag;
    }
    let wish = [(cy * fwd + sy * side) * SPEED, (sy * fwd - cy * side) * SPEED, 0.0];
    w.origin = quake_rs::world::walk_move(&w.bsp, w.origin, PLAYER_MINS, PLAYER_MAXS, wish, dt);

    let cam = Camera {
        pos: [w.origin[0], w.origin[1], w.origin[2] + 22.0],
        yaw: w.yaw,
        pitch: w.pitch,
        fov_deg: 90.0,
    };
    let instances: Vec<ModelInstance> = w
        .models
        .iter()
        .map(|(mdl, origin, yaw, color)| ModelInstance { mdl, origin: *origin, yaw: *yaw, color: *color, frame: 0 })
        .collect();
    render::render_scene(&w.bsp, &cam, W, H, &w.palette, &instances)
}

fn step_demo(d: &mut DemoPlay, dt: f32) -> render::Image {
    let n = d.demo.frames.len();
    let t0 = d.demo.frames[0].time;
    d.elapsed += dt;
    // Advance to the frame matching the recorded server time; loop at the end.
    while d.idx + 1 < n && (d.demo.frames[d.idx + 1].time - t0) <= d.elapsed {
        d.idx += 1;
    }
    if d.idx + 1 >= n {
        d.idx = 0;
        d.elapsed = 0.0;
    }
    let f = &d.demo.frames[d.idx];

    let mut owned: Vec<ModelInstance> = Vec::new();
    for e in &f.entities {
        if let Some(Some(mdl)) = d.models.get(e.modelindex) {
            owned.push(ModelInstance {
                mdl,
                origin: e.origin,
                yaw: e.angles[1],
                color: d.colors.get(e.modelindex).copied().unwrap_or([200, 200, 200]),
                // Demo entities carry their current animation frame from the net
                // stream — use it so monsters in the demo are actually posed.
                frame: e.frame.max(0) as usize,
            });
        }
    }
    let cam = Camera {
        pos: f.view_origin,
        yaw: f.view_angles[1],
        pitch: -f.view_angles[0],
        fov_deg: 90.0,
    };
    render::render_scene(&d.bsp, &cam, W, H, &d.palette, &owned)
}
