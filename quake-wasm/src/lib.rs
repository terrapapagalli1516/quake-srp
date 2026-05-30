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
use std::collections::HashMap;

use quake_rs::bsp::Bsp;
use quake_rs::demo::{parse_demo, Demo};
use quake_rs::dlight::DynamicLights;
use quake_rs::mdl::Mdl;
use quake_rs::pak::Pak;
use quake_rs::particles::{Lcg, ParticleSystem};
use quake_rs::progs::Progs;
use quake_rs::render::{self, Camera, Menu, MenuAction, MenuPics, ModelInstance, Viewmodel};
use quake_rs::server::{Server, TempEntityEvent, UserCmd};
use quake_rs::wad::Qpic;

static PAK: &[u8] = include_bytes!("../../quake-data/ID1/PAK0.PAK");

const WALK_MAP: &str = "maps/e1m1.bsp";
const DEMO_FILE: &str = "demo1.dem";
const W: usize = 320;
const H: usize = 200;
const SPEED: f32 = 320.0;

/// Interactive walk state: a live server ticked every frame, rendered from the
/// player edict. Monster thinks advance their animation frames and move them,
/// and the sound queue surfaces the events they fire.
struct Walk {
    server: Server,
    /// A second copy of the map BSP for rendering (the server owns its own copy
    /// inside the world host).
    bsp: Bsp,
    palette: [[u8; 3]; 256],
    /// The parsed `gfx.wad` (sbar + digit pics) for the status-bar HUD overlay,
    /// or `None` if the archive lacked/could not parse it. Parsed once at boot so
    /// the per-frame HUD draw is allocation-light.
    gfx_wad: Option<quake_rs::wad::Wad2>,
    /// The archive, kept open so sound samples load on demand as events fire.
    pak: Pak,
    /// Parsed alias models keyed by in-pak name (`None` = absent/unparseable).
    model_cache: HashMap<String, Option<Mdl>>,
    player: i32,
    yaw: f32,
    pitch: f32,
    in_fwd: f32,
    in_side: f32,
    in_attack: bool,
    /// A one-shot impulse (weapon switch etc.) queued by `set_impulse`, applied
    /// to the next `step_walk` UserCmd then cleared — matching how Quake's
    /// `impulse` console command fires once. 0 means "no impulse this frame".
    next_impulse: i32,
    /// Full-screen damage-flash intensity (Quake's `CSHIFT_DAMAGE` percent,
    /// 0..150): bumped when the player loses health/armour and faded each frame.
    damage_blend: f32,
    /// Player health+armour total last frame (NaN until known / after a level
    /// change), used to detect the damage taken this frame for the flash.
    last_total: f32,
    /// Accumulated game time (seconds), advanced by `dt` each `step_walk`. Drives
    /// the animated special surfaces: liquid warp + sky scroll in the renderer.
    clock: f32,
    /// Live engine particles (the `particle()` builtin's effect). Bursts the
    /// QuakeC fires each frame are drained into this pool, aged under gravity,
    /// and drawn into the scene sharing its z-buffer.
    particles: ParticleSystem,
    /// Deterministic RNG for particle spawns (no `rand` crate; std-only).
    prng: Lcg,
    /// Live dynamic lights (explosions, muzzle flashes, EF_* lights). Allocated
    /// each frame from the drained temp entities + the server's entity_dlights,
    /// decayed under `advance`, and passed to the renderer to light the walls.
    dlights: DynamicLights,
    /// The main-menu engine + its pre-loaded pics. Quake boots INTO the menu over
    /// the e1m1 frame; while `menu.visible`, gameplay input is gated (the world
    /// still idles) and `draw_menu` overlays the frame.
    menu: Menu,
    /// The menu's plaque/title/list/cursor pics, loaded once at boot from the pak.
    menu_pics: MenuPics,
    /// The 128x128 `conchars` font atlas (wrapped as a Qpic) for `draw_string`,
    /// or `None` if `gfx.wad`/conchars were absent.
    conchars: Option<Qpic>,
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

/// Load the main-menu pics from the pak's `.lmp` files (`Qpic::parse` on each)
/// plus the `conchars` font atlas from `gfx.wad`. Every pic is optional: a pak
/// missing any one leaves that slot `None` and the menu still draws the rest.
fn load_menu_pics(
    pak: &Pak,
    gfx_wad: Option<&quake_rs::wad::Wad2>,
) -> (MenuPics, Option<Qpic>) {
    let lmp = |n: &str| -> Option<Qpic> {
        pak.read_file(n).ok().flatten().and_then(|b| Qpic::parse(&b).ok())
    };
    let mut menudot: [Option<Qpic>; 6] = Default::default();
    for (i, slot) in menudot.iter_mut().enumerate() {
        *slot = lmp(&format!("gfx/menudot{}.lmp", i + 1));
    }
    let pics = MenuPics {
        qplaque: lmp("gfx/qplaque.lmp"),
        ttl_main: lmp("gfx/ttl_main.lmp"),
        mainmenu: lmp("gfx/mainmenu.lmp"),
        ttl_sgl: lmp("gfx/ttl_sgl.lmp"),
        sp_menu: lmp("gfx/sp_menu.lmp"),
        menudot,
    };

    // conchars is a raw 128x128 byte block (TYP_MIPTEX, no QPIC header) inside
    // gfx.wad. Wrap the 16384 lump bytes as a 128x128 Qpic for draw_string.
    let conchars = gfx_wad.and_then(|w| {
        let lump = w.lump("conchars")?;
        let data = w.lump_data(lump).ok()?;
        if data.len() < 128 * 128 {
            return None;
        }
        Some(Qpic {
            width: 128,
            height: 128,
            data: data[..128 * 128].to_vec(),
        })
    });

    (pics, conchars)
}

fn build_walk() -> Option<Walk> {
    let pak = pak()?;
    let read = |n: &str| pak.read_file(n).ok().flatten();
    let bsp = Bsp::parse(&read(WALK_MAP)?).ok()?;
    let bsp_sim = Bsp::parse(&read(WALK_MAP)?).ok()?;
    let palette = render::parse_palette(&read("gfx/palette.lmp")?)?;
    let progs = Progs::parse(&read("progs.dat")?).ok()?;
    // The HUD pics live in gfx.wad; parse it once (None if absent/unparseable).
    let gfx_wad = read("gfx.wad").and_then(|b| quake_rs::wad::Wad2::parse(b).ok());
    // The menu pics (.lmp files) + conchars (from gfx.wad), loaded once.
    let (menu_pics, conchars) = load_menu_pics(&pak, gfx_wad.as_ref());
    let (_spawn, yaw) = player_start(&bsp.entities).unwrap_or(([0.0, 0.0, 0.0], 0.0));

    // A live server: spawn the map's entities, then connect the local player.
    let mut server = Server::new(bsp_sim, progs).ok()?;
    server.spawn_entities().ok()?;
    let player = server.connect_client().ok()?;

    Some(Walk {
        server,
        bsp,
        palette,
        gfx_wad,
        pak,
        model_cache: HashMap::new(),
        player,
        yaw,
        pitch: 0.0,
        in_fwd: 0.0,
        in_side: 0.0,
        in_attack: false,
        next_impulse: 0,
        damage_blend: 0.0,
        last_total: f32::NAN,
        clock: 0.0,
        particles: ParticleSystem::new(),
        prng: Lcg::new(0x9E37_79B9),
        dlights: DynamicLights::new(),
        // Quake boots INTO the menu over the e1m1 frame.
        menu: {
            let mut m = Menu::new();
            m.open();
            m
        },
        menu_pics,
        conchars,
    })
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

/// Set whether the attack button is held (drives the QuakeC weapon code).
#[no_mangle]
pub extern "C" fn set_attack(on: i32) {
    ensure_app(|a| {
        if let Some(w) = a.walk.as_mut() {
            w.in_attack = on != 0;
        }
    });
}

/// Queue a one-shot impulse for the next frame (e.g. weapon select: 1 = axe,
/// 2 = shotgun, 3 = super shotgun, 4 = nailgun, ... — exactly the QuakeC
/// `impulse` numbers). Applied to the next `step_walk` UserCmd then cleared.
#[no_mangle]
pub extern "C" fn set_impulse(n: i32) {
    ensure_app(|a| {
        if let Some(w) = a.walk.as_mut() {
            w.next_impulse = n;
        }
    });
}

// --- main menu: keyboard navigation exports (ArrowUp/Down, Enter, Escape) ---

/// Move the menu cursor up one item (wraps), porting `K_UPARROW`. No-op when the
/// menu is hidden.
#[no_mangle]
pub extern "C" fn menu_up() {
    ensure_app(|a| {
        if let Some(w) = a.walk.as_mut() {
            if w.menu.visible {
                w.menu.move_cursor(-1);
            }
        }
    });
}

/// Move the menu cursor down one item (wraps), porting `K_DOWNARROW`. No-op when
/// the menu is hidden.
#[no_mangle]
pub extern "C" fn menu_down() {
    ensure_app(|a| {
        if let Some(w) = a.walk.as_mut() {
            if w.menu.visible {
                w.menu.move_cursor(1);
            }
        }
    });
}

/// Activate the highlighted menu item (Enter / `K_ENTER`). On
/// `MenuAction::NewGame` this rebuilds the walk on a fresh e1m1 (a new Server +
/// connected client) and closes the menu — the one-button "Single Player > New
/// Game". Other actions just update visibility/screen (handled inside `select`).
/// No-op when the menu is hidden.
#[no_mangle]
pub extern "C" fn menu_select() {
    // Decide the action under the borrow, then (if NewGame) rebuild the walk
    // afterward so we don't hold a &mut Walk while replacing it.
    let mut start_new_game = false;
    ensure_app(|a| {
        if let Some(w) = a.walk.as_mut() {
            if w.menu.visible {
                match w.menu.select() {
                    MenuAction::NewGame => start_new_game = true,
                    // Closed/Back/None already applied to the menu state inside
                    // select(); nothing else for the host to do.
                    _ => {}
                }
            }
        }
    });
    if start_new_game {
        // Fresh single-player game on e1m1 (NEW_GAME_MAP). Rebuild the whole walk
        // — new Server, new connected client — and leave the menu closed.
        if let Some(mut nw) = build_walk() {
            nw.menu.close();
            ensure_app(|a| {
                a.walk = Some(nw);
                a.mode = 0;
            });
        }
    }
}

/// Back out of the menu (Escape / `K_ESCAPE`): a submenu returns to the main
/// screen; the main screen closes the menu. If the menu is hidden, OPEN it (so
/// Escape always reaches the menu, like Quake's `M_ToggleMenu_f` for `key_game`).
#[no_mangle]
pub extern "C" fn menu_cancel() {
    ensure_app(|a| {
        if let Some(w) = a.walk.as_mut() {
            if w.menu.visible {
                let _ = w.menu.cancel();
            } else {
                w.menu.open();
            }
        }
    });
}

/// 1 when the menu is currently visible (capturing input), else 0. The page
/// reads this to route Arrow/Enter keys to the menu vs. the game.
#[no_mangle]
pub extern "C" fn menu_visible() -> i32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|a| a.walk.as_ref())
            .map(|w| w.menu.visible as i32)
            .unwrap_or(0)
    })
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

// --- sound: hand real Quake .wav bytes out of the pak for the page to play ---

/// Spatial parameters for one queued sound: its world emission point, volume
/// (`0.0..=1.0`) and attenuation (`0.0..=4.0`, where 0 = audible everywhere).
#[derive(Clone, Copy)]
struct SndParams {
    origin: [f32; 3],
    volume: f32,
    attenuation: f32,
}

impl SndParams {
    const fn zero() -> Self {
        SndParams { origin: [0.0; 3], volume: 0.0, attenuation: 0.0 }
    }
}

thread_local! {
    /// Scratch buffer the page reads via `sound_ptr` (for both the demo button
    /// and the per-frame queue below).
    static SND: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    /// WAV byte payloads for sounds fired this frame, awaiting playback, each
    /// paired with the spatial params the page reads to position it.
    static SND_QUEUE: RefCell<Vec<(Vec<u8>, SndParams)>> = const { RefCell::new(Vec::new()) };
    /// Spatial params of the entry the most recent `poll_sound` popped — the
    /// page reads these via the `sound_origin_*`/`sound_volume`/`sound_attenuation`
    /// exports after each non-zero `poll_sound`.
    static SND_CUR: RefCell<SndParams> = const { RefCell::new(SndParams::zero()) };
    /// The current listener pose, refreshed every walk `step`: eye position plus
    /// the forward and right unit vectors derived from the player's yaw. The page
    /// reads these via `listener_*` exports to spatialize each sound.
    static LISTENER: RefCell<Listener> = const { RefCell::new(Listener::zero()) };
}

/// Listener pose the page reads to spatialize queued sounds.
#[derive(Clone, Copy)]
struct Listener {
    pos: [f32; 3],
    forward: [f32; 3],
    right: [f32; 3],
}

impl Listener {
    const fn zero() -> Self {
        Listener { pos: [0.0; 3], forward: [0.0; 3], right: [0.0; 3] }
    }
}

/// Load the WAV bytes for the distinct gameplay sounds in `events` and push them
/// onto the playback queue. Constant ambient loops (`ambience/*`) and the silent
/// `misc/null.wav` are skipped, duplicates within the frame are collapsed, and
/// the queue is capped so a noisy frame can't grow it without bound.
fn queue_sounds(pak: &Pak, events: &[quake_rs::server::SoundEvent]) {
    if events.is_empty() {
        return;
    }
    let mut seen: Vec<&str> = Vec::new();
    SND_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        for ev in events {
            let name = ev.sample.as_str();
            if name.is_empty()
                || name == "misc/null.wav"
                || name.starts_with("ambience/")
                || seen.contains(&name)
                || q.len() >= 12
            {
                continue;
            }
            seen.push(name);
            if let Ok(Some(bytes)) = pak.read_file(name) {
                q.push((
                    bytes,
                    SndParams {
                        origin: ev.origin,
                        volume: ev.volume,
                        attenuation: ev.attenuation,
                    },
                ));
            }
        }
    });
}

/// Pop the next queued sound into the scratch buffer and return its byte length
/// (0 when the queue is empty). The page calls this in a loop each frame, reads
/// `sound_ptr()` after each non-zero return, and plays it via Web Audio. The
/// popped entry's spatial params are stashed for the `sound_origin_*` /
/// `sound_volume` / `sound_attenuation` exports to read alongside the bytes.
#[no_mangle]
pub extern "C" fn poll_sound() -> i32 {
    let next = SND_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if q.is_empty() {
            None
        } else {
            Some(q.remove(0))
        }
    });
    match next {
        Some((bytes, params)) => {
            let len = bytes.len() as i32;
            SND.with(|s| *s.borrow_mut() = bytes);
            SND_CUR.with(|p| *p.borrow_mut() = params);
            len
        }
        None => 0,
    }
}

/// Spatial params of the entry the most recent `poll_sound` popped. `origin_*`
/// are the world emission point; `volume` is `0.0..=1.0`; `attenuation` is
/// `0.0..=4.0` (0 = no falloff, audible everywhere). The page reads these after
/// each non-zero `poll_sound` to compute distance gain and stereo pan.
#[no_mangle]
pub extern "C" fn sound_origin_x() -> f32 {
    SND_CUR.with(|p| p.borrow().origin[0])
}
#[no_mangle]
pub extern "C" fn sound_origin_y() -> f32 {
    SND_CUR.with(|p| p.borrow().origin[1])
}
#[no_mangle]
pub extern "C" fn sound_origin_z() -> f32 {
    SND_CUR.with(|p| p.borrow().origin[2])
}
#[no_mangle]
pub extern "C" fn sound_volume() -> f32 {
    SND_CUR.with(|p| p.borrow().volume)
}
#[no_mangle]
pub extern "C" fn sound_attenuation() -> f32 {
    SND_CUR.with(|p| p.borrow().attenuation)
}

/// The listener (player) pose as of the last walk `step`: eye position and the
/// forward/right unit vectors derived from the player's yaw. The page reads
/// these to spatialize each sound (distance from `pos`, pan via dot with right).
#[no_mangle]
pub extern "C" fn listener_x() -> f32 {
    LISTENER.with(|l| l.borrow().pos[0])
}
#[no_mangle]
pub extern "C" fn listener_y() -> f32 {
    LISTENER.with(|l| l.borrow().pos[1])
}
#[no_mangle]
pub extern "C" fn listener_z() -> f32 {
    LISTENER.with(|l| l.borrow().pos[2])
}
#[no_mangle]
pub extern "C" fn listener_fwd_x() -> f32 {
    LISTENER.with(|l| l.borrow().forward[0])
}
#[no_mangle]
pub extern "C" fn listener_fwd_y() -> f32 {
    LISTENER.with(|l| l.borrow().forward[1])
}
#[no_mangle]
pub extern "C" fn listener_fwd_z() -> f32 {
    LISTENER.with(|l| l.borrow().forward[2])
}
#[no_mangle]
pub extern "C" fn listener_right_x() -> f32 {
    LISTENER.with(|l| l.borrow().right[0])
}
#[no_mangle]
pub extern "C" fn listener_right_y() -> f32 {
    LISTENER.with(|l| l.borrow().right[1])
}
#[no_mangle]
pub extern "C" fn listener_right_z() -> f32 {
    LISTENER.with(|l| l.borrow().right[2])
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

/// The explosion sound a rocket/grenade/tarbaby temp entity plays
/// (the C `cl_sfx_r_exp3` = `weapons/r_exp3.wav`).
const TE_EXPLOSION_SOUND: &str = "weapons/r_exp3.wav";

/// Realise one decoded [`TempEntityEvent`] into `particles`, porting the
/// effect-mapping half of `CL_ParseTEnt`: explosion types spawn a
/// 1024-particle [`ParticleSystem::spawn_explosion`] (and return the explosion
/// sound to play), impact types a `R_RunParticleEffect`-style burst with the
/// matching colour/count, splashes a small upward burst, and beams nothing.
/// Returns `Some(sound_name)` for the explosion types, else `None`.
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
            particles.spawn_burst(ev.pos, [0.0, 0.0, 1.0], 232, 20, now, rng);
            None
        }
        _ => None, // beam/lightning types: no effect here.
    }
}

/// Perform a deferred level transition: save the current player's spawn parms,
/// load `next_map` and a fresh `progs.dat` from the open pak, spawn the new
/// level's entities, and reconnect the client carrying its inventory. On any
/// parse/spawn/connect failure the current level is left untouched (the guards
/// below all early-`return` rather than panic), so a missing or corrupt next map
/// is non-fatal — the player keeps playing the level they are on.
fn try_changelevel(w: &mut Walk, next_map: &str) {
    // Save the outgoing player's inventory into parm1..parm16 (SV_SaveSpawnparms
    // -> SetChangeParms). Done before we touch the old server's world.
    let parms = w.server.save_spawn_parms();

    let read = |n: &str| w.pak.read_file(n).ok().flatten();
    // Two BSP copies (one for the sim/collision world the server owns, one for
    // rendering) plus a fresh progs.dat for the new server. Any failure aborts
    // the swap, leaving the live level running.
    let Some(map_bytes) = read(next_map) else { return };
    let Ok(sim_bsp) = Bsp::parse(&map_bytes) else { return };
    let Ok(render_bsp) = Bsp::parse(&map_bytes) else { return };
    let Some(progs_bytes) = read("progs.dat") else { return };
    let Ok(progs) = Progs::parse(&progs_bytes) else { return };

    let Ok(mut ns) = Server::new(sim_bsp, progs) else { return };
    if ns.spawn_entities().is_err() {
        return;
    }
    let Ok(player) = ns.connect_client_with_parms(parms) else { return };

    // Commit the swap. From here nothing can fail.
    let (_spawn, yaw) =
        player_start(&render_bsp.entities).unwrap_or(([0.0, 0.0, 0.0], w.yaw));
    w.server = ns;
    w.bsp = render_bsp;
    w.player = player;
    w.yaw = yaw;
    w.pitch = 0.0;

    // New level, clean slate: drop the old level's particles / dynamic lights and
    // reset the animation clock so liquids/sky restart from zero.
    w.particles = ParticleSystem::new();
    w.dlights = DynamicLights::new();
    w.clock = 0.0;
    // Reset the screen-blend state so the level change does not flash red.
    w.damage_blend = 0.0;
    w.last_total = f32::NAN;
    // Drop any events the *outgoing* server queued (the new server starts fresh).
    let _ = w.server.drain_sounds();
    let _ = w.server.drain_particles();
    let _ = w.server.drain_temp_entities();
}

fn step_walk(w: &mut Walk, dt: f32) -> render::Image {
    // Advance the animation clock (used for liquid warp + sky scroll). Guard
    // against a non-finite/negative dt so the clock only ever moves forward.
    if dt.is_finite() && dt > 0.0 {
        w.clock += dt;
    }

    // 1. Tick the live server with this frame's input. forwardmove/sidemove are
    //    Quake run speeds; the server's SV_ClientThink turns them into motion and
    //    runs every entity's think (so monsters animate and move).
    //
    //    While the menu is up, gate gameplay input: the world still TICKS (so it
    //    idles — monsters keep their think schedule, doors finish moving) but the
    //    player neither moves, fires, nor switches weapons. We send a zeroed
    //    UserCmd at the current view angles (Quake's `key_dest == key_menu` stops
    //    feeding the movement/attack/impulse commands the same way).
    let menu_up = w.menu.visible;
    let (mut fwd, mut side) = if menu_up { (0.0, 0.0) } else { (w.in_fwd, w.in_side) };
    let mag = (fwd * fwd + side * side).sqrt();
    if mag > 1.0 {
        fwd /= mag;
        side /= mag;
    }
    let cmd = UserCmd {
        forwardmove: fwd * SPEED,
        sidemove: side * SPEED,
        upmove: 0.0,
        yaw: w.yaw,
        pitch: w.pitch,
        buttons: if !menu_up && w.in_attack { 1 } else { 0 },
        impulse: if menu_up { 0 } else { w.next_impulse },
    };
    // A queued impulse fires once (the server also clears the edict field after
    // ImpulseCommands, but clearing here guarantees a held key fires a single
    // weapon switch rather than re-selecting every frame).
    w.next_impulse = 0;
    let _ = w.server.client_frame(&cmd, dt);

    // 1b. Level transition: a trigger_changelevel the player crossed this frame
    //     ran the QuakeC changelevel() builtin, which only *recorded* the next
    //     map (it cannot swap mid-frame). Now that the frame has finished, save
    //     the player's spawn parms (inventory) and swap to the new level,
    //     reconnecting the client so DecodeLevelParms restores the carried
    //     inventory. A missing/bad map leaves the current level running.
    if let Some(next_map) = w.server.take_pending_changelevel() {
        try_changelevel(w, &next_map);
        // The swap reset the world; render this frame from the *new* level so the
        // player never sees a frame straddling two maps.
    }

    // 2. Surface the sounds the world fired this frame (gunshots, doors, monster
    //    voices) to the page's audio queue.
    let events = w.server.drain_sounds();
    queue_sounds(&w.pak, &events);

    // 2b. Realise the particle() bursts the world fired this frame (explosions,
    //     blood, gibs) into the live pool, then age it under gravity and retire
    //     expired particles. Spawn uses the current game clock for absolute
    //     lifetimes; advance uses sv_gravity*0.05 as the particle gravity factor.
    let now = w.clock;
    for b in w.server.drain_particles() {
        w.particles.spawn_burst(b.org, b.dir, b.color, b.count, now, &mut w.prng);
    }
    // 2c. Realise the temp entities (rocket/grenade explosions, bullet/spike wall
    //     impacts) the world fired via the Write* builtins. Explosions also queue
    //     their `weapons/r_exp3.wav` sound through the SAME spatial-audio path the
    //     other sounds use, with the explosion's world position as its origin.
    let tents = w.server.drain_temp_entities();
    let mut te_sounds: Vec<quake_rs::server::SoundEvent> = Vec::new();
    for ev in &tents {
        // Explosions spawn a decaying dynamic light (CL_ParseTEnt): radius 350,
        // die now+0.5, decay 300, minlight 0, key 0 -> a fresh slot each one.
        {
            use quake_rs::server::te_consts::*;
            if matches!(ev.te_type, TE_EXPLOSION | TE_TAREXPLOSION | TE_EXPLOSION2) {
                w.dlights.alloc(0, ev.pos, 350.0, now + 0.5, 300.0, 0.0, now);
            }
        }
        if let Some(name) = spawn_temp_entity(&mut w.particles, ev, now, &mut w.prng) {
            te_sounds.push(quake_rs::server::SoundEvent {
                entity: 0,
                channel: 0,
                sound_index: -1,
                sample: name.to_string(),
                origin: ev.pos,
                volume: 1.0,
                attenuation: 1.0,
            });
        }
    }
    if !te_sounds.is_empty() {
        queue_sounds(&w.pak, &te_sounds);
    }
    // 2d. Entity light effects (EF_MUZZLEFLASH / BRIGHTLIGHT / DIMLIGHT) from the
    //     in-use edicts. The rand()&31 radius jitter is added here (entity_dlights
    //     stays a pure query). Then decay + retire the whole pool for this frame.
    for ed in w.server.entity_dlights() {
        let jitter = w.prng.next_range(32) as f32;
        w.dlights.alloc(
            ed.key,
            ed.origin,
            ed.radius_base + jitter,
            now + ed.life,
            0.0,
            ed.minlight,
            now,
        );
    }
    if dt.is_finite() && dt > 0.0 {
        w.particles.advance(dt, now, 800.0 * 0.05);
        w.dlights.advance(dt, now);
    }

    // 3. Make sure every live entity's alias model is cached (runtime-spawned
    //    entities — gibs, projectiles — can appear after boot).
    let n = w.server.vm.num_edicts();
    for e in 0..n {
        if w.server.vm.edict_free.get(e).copied().unwrap_or(true) {
            continue;
        }
        let m = w.server.vm.ent_get_string(e as i32, "model");
        if m.ends_with(".mdl") && !w.model_cache.contains_key(&m) {
            let parsed = w.pak.read_file(&m).ok().flatten().and_then(|b| Mdl::parse(&b).ok());
            w.model_cache.insert(m, parsed);
        }
    }

    // The player's first-person weapon viewmodel ("progs/v_shot.mdl" etc.) lives
    // on the `weaponmodel` field (separate from `model`); cache it like any MDL.
    let weapon_name = w.server.vm.ent_get_string(w.player, "weaponmodel");
    if weapon_name.ends_with(".mdl") && !w.model_cache.contains_key(&weapon_name) {
        let parsed = w.pak.read_file(&weapon_name).ok().flatten().and_then(|b| Mdl::parse(&b).ok());
        w.model_cache.insert(weapon_name.clone(), parsed);
    }
    let weapon_frame = w.server.vm.ent_get_float(w.player, "weaponframe").max(0.0) as usize;

    // 4. Gather the visible entities (owned descriptors, so the cache borrow for
    //    rendering doesn't clash with reading the server). Skip the player's own
    //    edict — its model would fill the screen in first person.
    let mut descs: Vec<(String, [f32; 3], f32, usize, [u8; 3])> = Vec::new();
    let mut bmodels: Vec<render::BModelInstance> = Vec::new();
    for e in 0..n {
        let ent = e as i32;
        if ent == w.player || w.server.vm.edict_free.get(e).copied().unwrap_or(true) {
            continue;
        }
        let m = w.server.vm.ent_get_string(ent, "model");
        // Brush submodels (doors, platforms, buttons) draw at the entity origin —
        // their origin tracks the door's open/close motion, so they animate live.
        if let Some(num) = m.strip_prefix('*') {
            if let Ok(idx) = num.parse::<usize>() {
                let origin = w.server.vm.ent_get_vector(ent, "origin");
                bmodels.push(render::BModelInstance { model_index: idx, origin });
            }
            continue;
        }
        if !m.ends_with(".mdl") {
            continue;
        }
        let origin = w.server.vm.ent_get_vector(ent, "origin");
        let yaw = w.server.vm.ent_get_vector(ent, "angles")[1];
        let frame = w.server.vm.ent_get_float(ent, "frame").max(0.0) as usize;
        let color = color_for_name(&m);
        descs.push((m, origin, yaw, frame, color));
    }

    // 5. Render from the player's eye, with Quake's head-bob added to the eye
    //    height (V_CalcBob) so the view rocks as the player moves.
    let (mut eye, ang) = w.server.player_view();
    let vel = w.server.vm.ent_get_vector(w.player, "velocity");
    let speed_xy = (vel[0] * vel[0] + vel[1] * vel[1]).sqrt();
    let bob = render::view_bob(speed_xy, w.clock);

    // Record the listener pose so the page can spatialize this frame's queued
    // sounds. Forward/right are the level (no-pitch) yaw basis, matching the
    // renderer's `Camera::basis`: yaw rotates in XY about +Z, right is forward
    // turned -90 deg. Panning only needs the horizontal plane.
    let yaw_rad = (ang[1] as f64).to_radians();
    let (sy, cy) = (yaw_rad.sin() as f32, yaw_rad.cos() as f32);
    LISTENER.with(|l| {
        *l.borrow_mut() = Listener {
            pos: eye,
            forward: [cy, sy, 0.0],
            right: [sy, -cy, 0.0],
        };
    });

    // Bob the rendered eye only (the listener pose above stays steady so audio
    // panning does not jitter with the head-bob).
    eye[2] += bob;
    let cam = Camera {
        pos: eye,
        yaw: ang[1],
        pitch: -ang[0], // QuakeC pitch is +down; the renderer's is +up.
        fov_deg: 90.0,
    };
    let instances: Vec<ModelInstance> = descs
        .iter()
        .filter_map(|(name, origin, yaw, frame, color)| match w.model_cache.get(name) {
            Some(Some(mdl)) => Some(ModelInstance {
                mdl,
                origin: *origin,
                yaw: *yaw,
                color: *color,
                frame: *frame,
            }),
            _ => None,
        })
        .collect();
    // Anchor the weapon viewmodel to the camera (drawn last, on top of the world).
    let viewmodel = match w.model_cache.get(&weapon_name) {
        Some(Some(mdl)) => Some(Viewmodel { mdl, frame: weapon_frame }),
        _ => None,
    };
    // The live particles as (world pos, palette index); they share the scene
    // z-buffer so any behind a wall are correctly hidden.
    let parts: Vec<([f32; 3], u8)> =
        w.particles.particles().iter().map(|p| (p.origin, p.color)).collect();
    // The live dynamic lights (explosions / muzzle flashes) light up nearby walls.
    let active_dlights = w.dlights.active();
    // The animated light-style scales (torch flicker, pulsing lights) at the
    // current server clock; the worldspawn populated the styles at spawn time.
    let light_styles = w.server.lightstyle_scales(w.clock);
    let mut img =
        render::render_scene_ext(&w.bsp, &cam, W, H, &w.palette, &instances, &bmodels, viewmodel, w.clock, &parts, &active_dlights, &light_styles);

    // 5b. Screen blends (V_CalcBlend): fade the damage flash, bump it when the
    //     player lost health/armour this frame, and tint the view when the eye is
    //     under water / in lava or slime. Applied to the 3-D frame BEFORE the HUD
    //     (Quake never tints the status bar).
    w.damage_blend = (w.damage_blend - dt * 150.0).max(0.0);
    let total = w.server.vm.ent_get_float(w.player, "health")
        + w.server.vm.ent_get_float(w.player, "armorvalue");
    if w.last_total.is_finite() {
        let lost = (w.last_total - total).max(0.0);
        if lost > 0.0 {
            w.damage_blend = (w.damage_blend + 3.0 * lost).min(150.0);
        }
    }
    w.last_total = total;
    let mut shifts: Vec<([u8; 3], f32)> = Vec::new();
    if w.damage_blend > 0.0 {
        shifts.push(([255, 0, 0], w.damage_blend));
    }
    if let Some(cs) = render::content_cshift(quake_rs::world::point_contents(&w.bsp, eye)) {
        shifts.push(cs);
    }
    if !shifts.is_empty() {
        let (bc, ba) = render::combine_cshifts(&shifts);
        render::apply_blend(&mut img, bc, ba);
    }

    // 6. Status bar (HUD) overlay: blit the bottom bar with the player's live
    //    health/ammo/armour on top of the finished 3-D frame. Skipped silently
    //    when gfx.wad was absent (the world still renders).
    if let Some(wad) = w.gfx_wad.as_ref() {
        let stat = |f: &str| w.server.vm.ent_get_float(w.player, f) as i32;
        let hud = render::Hud {
            wad,
            palette: &w.palette,
            health: stat("health"),
            ammo: stat("ammo_shells"),
            armor: stat("armorvalue"),
        };
        render::draw_hud_into(&mut img, &hud);
    }

    // 7. Main menu overlay: Quake boots into it and it stays on top of the game
    //    frame (world + HUD) until dismissed. Drawn last so it sits above
    //    everything. Skipped silently when not visible.
    if w.menu.visible {
        render::draw_menu(
            &mut img,
            &w.menu,
            &w.menu_pics,
            w.conchars.as_ref(),
            w.clock,
            &w.palette,
        );
    }
    img
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
    // The recorded server time animates the demo's liquids/sky too. Recorded
    // demos carry no engine-particle stream or dynamic lights here, so those
    // slices are empty; and a demo has no live server to source light styles, so
    // pass the neutral (static) scales.
    render::render_scene_ext(&d.bsp, &cam, W, H, &d.palette, &owned, &[], None, f.time, &[], &[], &render::NEUTRAL_LIGHTSTYLE_SCALES)
}
