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
use quake_rs::render::{
    self, Camera, Console, Menu, MenuAction, MenuPics, ModelInstance, Viewmodel,
};
use quake_rs::server::{Server, TempEntityEvent, UserCmd};
use quake_rs::wad::Qpic;

static PAK: &[u8] = include_bytes!("../../quake-data/ID1/PAK0.PAK");

const WALK_MAP: &str = "maps/e1m1.bsp";
const DEMO_FILE: &str = "demo1.dem";
/// The default (boot) render resolution: Quake's fast 320x200. The engine boots
/// here; the Options menu lets the player opt into a larger framebuffer at runtime
/// (the menu + HUD auto-scale to whatever size they're drawn into).
const DEFAULT_W: usize = 320;
const DEFAULT_H: usize = 200;
/// Sane bounds for [`set_resolution`] (and the menu presets): the framebuffer is
/// clamped to this envelope and its total pixel count capped so a runaway value
/// cannot allocate gigabytes. `1280*800*4` bytes ≈ 4 MB is the upper bound.
const MIN_W: i32 = 320;
const MAX_W: i32 = 1280;
const MIN_H: i32 = 200;
const MAX_H: i32 = 800;
const MAX_PIXELS: i32 = 1280 * 800;
const SPEED: f32 = 320.0;

/// Clamp a requested `(w, h)` render resolution into the supported envelope:
/// width `MIN_W..=MAX_W`, height `MIN_H..=MAX_H`, and the total pixel count capped
/// at [`MAX_PIXELS`] (shrinking the height first if `w*h` would exceed it). Always
/// returns a valid, non-zero size — never panics on absurd input.
fn clamp_resolution(w: i32, h: i32) -> (usize, usize) {
    let mut cw = w.clamp(MIN_W, MAX_W);
    let mut ch = h.clamp(MIN_H, MAX_H);
    // Cap the pixel budget so a wide AND tall request can't blow the cap even
    // though each dimension is individually in range. Trim the height to fit,
    // never below its minimum.
    if cw.saturating_mul(ch) > MAX_PIXELS {
        ch = (MAX_PIXELS / cw.max(1)).clamp(MIN_H, MAX_H);
        // If even MIN_H * cw overflows the cap (it can't with these constants,
        // but stay safe), trim the width too.
        if cw.saturating_mul(ch) > MAX_PIXELS {
            cw = (MAX_PIXELS / ch.max(1)).clamp(MIN_W, MAX_W);
        }
    }
    (cw as usize, ch as usize)
}

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
    /// `gfx/colormap.lmp` — the 64x256 shade LUT for faithful colormap-indexed
    /// wall lighting (Quake never overbrights). `None` falls back to the linear
    /// brightness multiply. Loaded once at boot.
    colormap: Option<Vec<u8>>,
    /// The archive, kept open so sound samples load on demand as events fire.
    pak: Pak,
    /// Parsed alias models keyed by in-pak name (`None` = absent/unparseable).
    model_cache: HashMap<String, Option<Mdl>>,
    /// Parsed *external brush* models keyed by in-pak name (`None` =
    /// absent/unparseable). These are Quake's standalone `maps/b_*.bsp` item
    /// boxes (explosive box, ammo/health boxes) that items `setmodel()` to at
    /// runtime. Cached like `model_cache` so a box parses once and backs every
    /// instance of that item; rendered via [`render::ExternalBModel`].
    bmodel_cache: HashMap<String, Option<Bsp>>,
    /// Per-entity previous render origin, keyed by edict index — the source point
    /// for R_RocketTrail (rockets/grenades/gibs trail from their old origin to the
    /// new one each frame). Defaults to the current origin the first time an
    /// entity is seen, so there's no spurious trail on spawn.
    trail_org: HashMap<i32, [f32; 3]>,
    /// Quake's `tracercount` (CL_RelinkEntities `static int`): alternates the
    /// tracer-trail offset direction; threaded across `spawn_rocket_trail` calls.
    tracercount: u32,
    /// The world map's in-pak path (e.g. `maps/e1m1.bsp`). An entity whose
    /// `model` equals this is the worldspawn brush — never loaded as an external
    /// box (it is already drawn as the world).
    map_name: String,
    player: i32,
    yaw: f32,
    pitch: f32,
    in_fwd: f32,
    in_side: f32,
    in_attack: bool,
    /// Whether the jump key is held (UserCmd button bit 1 -> the player's
    /// `button2`, which the QuakeC PlayerJump reads to leap when on the ground).
    in_jump: bool,
    /// Whether the swim-down key (`c`) is held: drives `UserCmd.upmove` negative,
    /// which `SV_WaterMove` reads to sink. Ignored out of water (the WALK air
    /// move zeroes the vertical wish). Swim-UP reuses `in_jump` (Space) the same
    /// way — in water it pushes up, on land it just jumps.
    in_down: bool,
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
    /// Live particles replayed from the recorded `svc_particle` / temp-entity
    /// stream: each frame's effects are spawned ONCE when playback advances onto
    /// it, then the pool is aged under gravity and drawn into the scene (sharing
    /// its z-buffer) — so the demo shows blood, gunshot puffs and explosions just
    /// like [`step_walk`] does for live play.
    particles: ParticleSystem,
    /// Deterministic RNG for the demo's particle spawns (std-only, like Walk).
    prng: Lcg,
    /// The frame index whose effects were last spawned, so a frame rendered for
    /// several steps spawns its bursts only on the step that ADVANCES onto it
    /// (never re-spawning while it lingers). `usize::MAX` = "none spawned yet".
    last_spawned_idx: usize,
}

struct App {
    walk: Option<Walk>,
    demo: Option<DemoPlay>,
    /// 0 = walk, 1 = demo.
    mode: u8,
    /// The main-menu engine. Lives at the App level (mode-independent) so it can
    /// overlay WHATEVER is playing — the walk OR the attract demo. Quake boots
    /// INTO the menu over the playing attract demo; while `menu.visible`,
    /// gameplay input is gated (the world still idles) and the `step` dispatcher
    /// overlays `draw_menu` on the finished frame.
    menu: Menu,
    /// The menu's plaque/title/list/cursor pics, loaded once on first boot from
    /// the pak (they are mode-independent). `None` until `ensure_menu_assets`
    /// runs once.
    menu_pics: MenuPics,
    /// The 128x128 `conchars` font atlas (wrapped as a Qpic) for `draw_string`,
    /// or `None` if `gfx.wad`/conchars were absent. Loaded alongside `menu_pics`.
    conchars: Option<Qpic>,
    /// Whether the menu assets (`menu_pics` + `conchars`) have been loaded yet.
    /// `ensure_menu_assets` loads them once on first boot; subsequent boots reuse
    /// them (they never change).
    menu_loaded: bool,
    /// The drop-down console (toggled with `~`). Mode-independent like the menu:
    /// it overlays whatever is playing and, while open, owns the keyboard. Its
    /// commands act on the live [`Walk`].
    console: Console,
    /// The `gfx/conback.lmp` console background (a 320x200 QPIC), loaded once
    /// alongside the menu assets. `None` if the pak lacked it — `draw_console`
    /// then falls back to a dark fill.
    conback: Option<Qpic>,
    /// Accumulated wall-clock time (seconds), advanced by `dt` each `step`
    /// regardless of mode. Drives the menu cursor animation (Quake's `host_time`
    /// in `M_DrawCursor`), which must keep blinking over a frozen frame too.
    clock: f32,
    /// Current render resolution (runtime; defaults to [`DEFAULT_W`] x
    /// [`DEFAULT_H`]). The scene renders at this size and the framebuffer is
    /// `render_w * render_h * 4` RGBA bytes, reallocated whenever it changes.
    render_w: usize,
    render_h: usize,
    fb: Vec<u8>, // RGBA, render_w*render_h*4
}

impl App {
    /// Resize the framebuffer to the (already-clamped) `(w, h)`, reallocating only
    /// when the size actually changes. The new buffer is zero-filled; the next
    /// `step` paints it.
    fn set_render_size(&mut self, w: usize, h: usize) {
        if self.render_w == w && self.render_h == h {
            return;
        }
        self.render_w = w;
        self.render_h = h;
        // `w*h*4` is bounded by MAX_PIXELS*4 (~4 MB) after clamping, so this can't
        // OOM; saturating_mul keeps us safe even if a caller bypassed the clamp.
        self.fb = vec![0u8; w.saturating_mul(h).saturating_mul(4)];
    }

    /// Load the menu pics + conchars from the pak ONCE (they are mode-independent
    /// and never change), the first time any boot needs them. A missing/bad pak
    /// leaves the slots empty — `draw_menu` then simply skips the absent pics.
    fn ensure_menu_assets(&mut self) {
        if self.menu_loaded {
            return;
        }
        self.menu_loaded = true;
        if let Some(pak) = pak() {
            let gfx_wad = pak
                .read_file("gfx.wad")
                .ok()
                .flatten()
                .and_then(|b| quake_rs::wad::Wad2::parse(b).ok());
            let (pics, conchars) = load_menu_pics(&pak, gfx_wad.as_ref());
            self.menu_pics = pics;
            self.conchars = conchars;
            // The console background (gfx/conback.lmp): a raw 320x200 QPIC.
            // Optional — a pak missing it leaves draw_console's dark-fill fallback.
            self.conback = pak
                .read_file("gfx/conback.lmp")
                .ok()
                .flatten()
                .and_then(|b| Qpic::parse(&b).ok());
        }
    }

    /// The palette of the active mode (the walk's, or the demo's), for the menu
    /// overlay. `None` when no mode has a scene yet (then there is nothing to
    /// overlay the menu onto anyway).
    fn active_palette(&self) -> Option<&[[u8; 3]; 256]> {
        if self.mode == 1 {
            self.demo.as_ref().map(|d| &d.palette)
        } else {
            self.walk.as_ref().map(|w| &w.palette)
        }
    }
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
        p_option: lmp("gfx/p_option.lmp"),
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
    build_walk_map(WALK_MAP)
}

/// Build a live walk on `map` (a `maps/*.bsp` pak path): boot uses [`WALK_MAP`];
/// New Game uses [`render::NEW_GAME_MAP`] (the `start` hub).
fn build_walk_map(map: &str) -> Option<Walk> {
    let pak = pak()?;
    let read = |n: &str| pak.read_file(n).ok().flatten();
    let bsp = Bsp::parse(&read(map)?).ok()?;
    let bsp_sim = Bsp::parse(&read(map)?).ok()?;
    let palette = render::parse_palette(&read("gfx/palette.lmp")?)?;
    let progs = Progs::parse(&read("progs.dat")?).ok()?;
    // The HUD pics live in gfx.wad; parse it once (None if absent/unparseable).
    let gfx_wad = read("gfx.wad").and_then(|b| quake_rs::wad::Wad2::parse(b).ok());
    let colormap = read("gfx/colormap.lmp");
    let (_spawn, yaw) = player_start(&bsp.entities).unwrap_or(([0.0, 0.0, 0.0], 0.0));

    // A live server: spawn the map's entities, then connect the local player.
    // Pass the pak so external brush-model item boxes (b_*.bsp) collide + take
    // damage (the explosive box becomes shootable).
    let mut server = Server::with_pak(bsp_sim, progs, Some(pak.clone())).ok()?;
    server.spawn_entities().ok()?;
    let player = server.connect_client().ok()?;

    Some(Walk {
        server,
        bsp,
        palette,
        gfx_wad,
        colormap,
        pak,
        model_cache: HashMap::new(),
        bmodel_cache: HashMap::new(),
        trail_org: HashMap::new(),
        tracercount: 0,
        map_name: map.to_string(),
        player,
        yaw,
        pitch: 0.0,
        in_fwd: 0.0,
        in_side: 0.0,
        in_attack: false,
        in_jump: false,
        in_down: false,
        next_impulse: 0,
        damage_blend: 0.0,
        last_total: f32::NAN,
        clock: 0.0,
        particles: ParticleSystem::new(),
        prng: Lcg::new(0x9E37_79B9),
        dlights: DynamicLights::new(),
    })
}

fn build_demo() -> Option<DemoPlay> {
    let pak = pak()?;
    let read = |n: &str| pak.read_file(n).ok().flatten();
    let demo_bytes = read(DEMO_FILE)?;
    let demo = parse_demo(&demo_bytes).ok()?;
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
    // Re-parse with inter-frame interpolation — smooth 60 fps playback instead of
    // the choppy 10 Hz keyframes (CL_LerpPoint), plus EF_ROTATE spin for any model
    // whose header flags it (rotating pickups). The set of rotating model indices
    // is derived from the just-loaded MDL headers.
    let rotating: Vec<usize> = models
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            m.as_ref()
                .filter(|md| md.header.flags & quake_rs::demo::EF_ROTATE != 0)
                .map(|_| i)
        })
        .collect();
    let demo = quake_rs::demo::parse_demo_interpolated(&demo_bytes, 60.0, &rotating).ok()?;
    if demo.frames.is_empty() {
        return None;
    }
    Some(DemoPlay {
        bsp,
        palette,
        demo,
        models,
        colors,
        elapsed: 0.0,
        idx: 0,
        particles: ParticleSystem::new(),
        prng: Lcg::new(0x9E37_79B9),
        last_spawned_idx: usize::MAX,
    })
}

fn ensure_app(f: impl FnOnce(&mut App)) {
    APP.with(|c| {
        if c.borrow().is_none() {
            *c.borrow_mut() = Some(App {
                walk: None,
                demo: None,
                mode: 0,
                menu: Menu::new(),
                menu_pics: MenuPics::default(),
                conchars: None,
                menu_loaded: false,
                console: Console::new(),
                conback: None,
                clock: 0.0,
                render_w: DEFAULT_W,
                render_h: DEFAULT_H,
                fb: vec![0u8; DEFAULT_W * DEFAULT_H * 4],
            });
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
    // Clean slate: drop any sounds still queued from a previous mode so stale
    // samples can't play after the switch.
    SND_QUEUE.with(|q| q.borrow_mut().clear());
    let w = build_walk();
    let ok = w.is_some();
    ensure_app(|a| {
        a.ensure_menu_assets();
        // Only enter walk mode when the level actually built; otherwise leave
        // the current mode untouched (mirrors boot_demo's success gate) so a
        // failed boot doesn't strand the app in walk mode with no Walk.
        if let Some(w) = w {
            a.walk = Some(w);
            a.mode = 0;
            // Quake boots INTO the menu over the e1m1 frame. Reset the App-level
            // menu to fresh defaults (preset 0) and open it over the walk, the
            // same clean slate the old fresh-Walk-with-fresh-Menu boot gave.
            a.menu = Menu::new();
            a.menu.open();
            // The fresh menu is at resolution preset 0 (DEFAULT); reset the App
            // render size to match so the Options "Screen size" label and the
            // actual framebuffer never desync after a re-boot.
            a.set_render_size(DEFAULT_W, DEFAULT_H);
        }
    });
    ok as i32
}

/// Start recorded-demo playback (demo1.dem / e1m3). Returns 1 on success.
#[no_mangle]
pub extern "C" fn boot_demo() -> i32 {
    // Clean slate: drop any sounds still queued from a previous mode.
    SND_QUEUE.with(|q| q.borrow_mut().clear());
    let d = build_demo();
    let ok = d.is_some();
    ensure_app(|a| {
        a.ensure_menu_assets();
        a.demo = d;
        if a.demo.is_some() {
            a.mode = 1;
            // The demo button plays the demo with the menu CLOSED (clean
            // playback). `boot_attract` is the variant that opens the menu over it.
            // Reset to fresh defaults so the menu's Options preset matches the
            // DEFAULT framebuffer we set below.
            a.menu = Menu::new();
            a.set_render_size(DEFAULT_W, DEFAULT_H);
        }
    });
    ok as i32
}

/// Boot into the ATTRACT loop: start recorded-demo playback (demo1.dem) with the
/// main menu OPEN over it — exactly how Quake boots (the menu draws on top of the
/// playing demo, the "attract" screen). The page calls this on load instead of
/// [`boot`]. Returns 1 when the demo built (menu over the playing demo), or 0 when
/// it could not — in which case we fall back to [`boot`] so the user still lands
/// on a menu over *something* (e1m1) rather than a blank screen.
#[no_mangle]
pub extern "C" fn boot_attract() -> i32 {
    // Clean slate: drop any sounds still queued from a previous mode.
    SND_QUEUE.with(|q| q.borrow_mut().clear());
    let d = build_demo();
    let built = d.is_some();
    ensure_app(|a| {
        a.ensure_menu_assets();
        if let Some(d) = d {
            a.demo = Some(d);
            a.mode = 1;
            // The menu overlays the PLAYING attract demo. Fresh defaults (preset 0)
            // keep the Options "Screen size" label in sync with the DEFAULT fb.
            a.menu = Menu::new();
            a.menu.open();
            a.set_render_size(DEFAULT_W, DEFAULT_H);
        }
    });
    if built {
        1
    } else {
        // No demo (missing/bad pak): still give the player a menu over a frame.
        boot()
    }
}

/// The current render width in pixels (defaults to [`DEFAULT_W`] = 320). The page
/// reads this each frame and resizes its canvas backing store + ImageData when it
/// changes (e.g. after the Options menu picks a larger preset).
#[no_mangle]
pub extern "C" fn width() -> i32 {
    APP.with(|c| c.borrow().as_ref().map(|a| a.render_w as i32).unwrap_or(DEFAULT_W as i32))
}
/// The current render height in pixels (defaults to [`DEFAULT_H`] = 200).
#[no_mangle]
pub extern "C" fn height() -> i32 {
    APP.with(|c| c.borrow().as_ref().map(|a| a.render_h as i32).unwrap_or(DEFAULT_H as i32))
}

/// Set the render resolution at runtime, reallocating the framebuffer. The
/// requested `(w, h)` is clamped to the supported envelope (width 320..=1280,
/// height 200..=800, and total pixels <= 1_280*800 so a runaway can't OOM) via
/// [`clamp_resolution`]; out-of-range input is clamped, never a panic. After this,
/// `width()`/`height()` report the new (clamped) size and the next `step` renders
/// the scene at it. The menu + HUD auto-scale to the new framebuffer size.
#[no_mangle]
pub extern "C" fn set_resolution(w: i32, h: i32) {
    let (cw, ch) = clamp_resolution(w, h);
    ensure_app(|a| a.set_render_size(cw, ch));
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

/// Set whether the jump key is held. Maps to UserCmd button bit 1 -> the
/// player's `button2`, which the QuakeC PlayerJump reads to jump when on the
/// ground (velocity_z = 270).
#[no_mangle]
pub extern "C" fn set_jump(on: i32) {
    ensure_app(|a| {
        if let Some(w) = a.walk.as_mut() {
            w.in_jump = on != 0;
        }
    });
}

/// Set whether the swim-DOWN key (`c`, the `+movedown` key) is held. Maps to a
/// negative `UserCmd.upmove`, which `SV_WaterMove` reads to sink while waist-deep
/// in water. Out of water it has no effect (the walk move ignores upmove).
#[no_mangle]
pub extern "C" fn set_movedown(on: i32) {
    ensure_app(|a| {
        if let Some(w) = a.walk.as_mut() {
            w.in_down = on != 0;
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
        if a.menu.visible {
            a.menu.move_cursor(-1);
        }
    });
}

/// Move the menu cursor down one item (wraps), porting `K_DOWNARROW`. No-op when
/// the menu is hidden.
#[no_mangle]
pub extern "C" fn menu_down() {
    ensure_app(|a| {
        if a.menu.visible {
            a.menu.move_cursor(1);
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
        if a.menu.visible {
            match a.menu.select() {
                MenuAction::NewGame => start_new_game = true,
                // Closed/Back/None already applied to the menu state inside
                // select(); nothing else for the host to do.
                _ => {}
            }
        }
    });
    if start_new_game {
        // Fresh single-player game on the start hub (NEW_GAME_MAP). Rebuild the
        // whole walk — new Server, new connected client — switch to walk mode and
        // leave the menu closed. From the hub the player picks skill + episode
        // (changelevel).
        if let Some(nw) = build_walk_map(render::NEW_GAME_MAP) {
            ensure_app(|a| {
                a.walk = Some(nw);
                a.mode = 0;
                // Reset the menu to fresh defaults and leave it closed — exactly
                // what the old fresh-Walk-with-fresh-Menu rebuild did. This also
                // returns the Options "Screen size" preset to 0, so the
                // render-size reset to DEFAULT below stays in sync with the label.
                a.menu = Menu::new();
                // Fresh menu = resolution preset 0; keep the App render size in
                // sync so the Options label and framebuffer don't desync.
                a.set_render_size(DEFAULT_W, DEFAULT_H);
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
        if a.menu.visible {
            let _ = a.menu.cancel();
        } else {
            a.menu.open();
        }
    });
}

/// Adjust the highlighted Options row leftward (decrement / cycle back), porting
/// `K_LEFTARROW` on the options screen. A no-op unless the menu is visible AND on
/// the Options screen ([`Menu::adjust`] itself enforces the latter). If the
/// "Screen size" row changed, the framebuffer is reallocated to the menu's new
/// [`Menu::resolution`] so `width()`/`height()` and the next `step` follow it.
#[no_mangle]
pub extern "C" fn menu_left() {
    menu_adjust(-1);
}

/// Adjust the highlighted Options row rightward (increment / cycle forward),
/// porting `K_RIGHTARROW`. See [`menu_left`] for the resolution-apply behaviour.
#[no_mangle]
pub extern "C" fn menu_right() {
    menu_adjust(1);
}

/// Shared body of [`menu_left`]/[`menu_right`]: adjust the Options row under the
/// borrow, and if the Screen-size row changed, capture the new size and resize the
/// framebuffer afterward (so we don't hold the borrow while touching the App's
/// fb). No-op when the menu is hidden.
fn menu_adjust(delta: i32) {
    let mut new_size: Option<(usize, usize)> = None;
    ensure_app(|a| {
        if a.menu.visible && a.menu.adjust(delta) {
            // The Screen-size row changed: read the new (clamped) resolution.
            let (rw, rh) = a.menu.resolution();
            new_size = Some(clamp_resolution(rw, rh));
        }
    });
    if let Some((w, h)) = new_size {
        ensure_app(|a| a.set_render_size(w, h));
    }
}

/// The Options "Mouse speed" as a sensitivity multiplier (default 1.0). The page
/// multiplies its baseline look sensitivity by this. Reads from the App-level menu;
/// 1.0 when the app has not been created yet.
#[no_mangle]
pub extern "C" fn mouse_sensitivity() -> f32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| a.menu.mouse_sensitivity())
            .unwrap_or(1.0)
    })
}

/// The Options "Volume" as a `0.0..=1.0` master gain (default 0.7). The page
/// scales its sound gains by this. Reads from the App-level menu; 1.0 when the app
/// has not been created yet (so audio is never accidentally silenced before then).
#[no_mangle]
pub extern "C" fn volume() -> f32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| a.menu.volume())
            .unwrap_or(1.0)
    })
}

/// 1 when the menu is currently visible (capturing input), else 0. The page
/// reads this to route Arrow/Enter keys to the menu vs. the game.
#[no_mangle]
pub extern "C" fn menu_visible() -> i32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| a.menu.visible as i32)
            .unwrap_or(0)
    })
}

// --- drop-down console: toggle / typing / execution exports (the `~` key) ---

/// Toggle the drop-down console (the `~` / backtick key, Quake's
/// `Con_ToggleConsole_f`). Opening slides the panel down over whatever is
/// playing; closing slides it back. While open the console owns the keyboard.
#[no_mangle]
pub extern "C" fn console_toggle() {
    ensure_app(|a| a.console.toggle());
}

/// `1` when the console is open (capturing the keyboard), else `0`. The page
/// reads this to route keys to the console instead of the game / menu.
#[no_mangle]
pub extern "C" fn console_visible() -> i32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| a.console.open as i32)
            .unwrap_or(0)
    })
}

/// Append one typed character to the console input line. `code` is a Unicode
/// scalar value (the page passes `key.charCodeAt(0)` / `key.codePointAt(0)`).
/// Non-printable codes, the backtick/tilde (the toggle key), and anything while
/// the console is closed are ignored. A no-op once the input line is full.
#[no_mangle]
pub extern "C" fn console_char(code: u32) {
    ensure_app(|a| {
        if !a.console.open {
            return;
        }
        // Reject invalid scalar values; `putchar` further filters control chars
        // and the backtick/tilde toggle key.
        if let Some(ch) = char::from_u32(code) {
            a.console.putchar(ch);
        }
    });
}

/// Delete the last character of the console input line (Backspace). A no-op when
/// the console is closed or the line is empty.
#[no_mangle]
pub extern "C" fn console_backspace() {
    ensure_app(|a| {
        if a.console.open {
            a.console.backspace();
        }
    });
}

/// Submit the console input line (Enter): echo it into the scrollback and
/// execute it against the live game. A no-op when the console is closed or the
/// line is blank. The command may swap the level (`map`) and close the console.
#[no_mangle]
pub extern "C" fn console_enter() {
    // Take the line under the borrow, then execute it (execute_console_command
    // borrows the App again to touch the walk / open-state).
    let line = APP.with(|c| {
        c.borrow_mut()
            .as_mut()
            .filter(|a| a.console.open)
            .and_then(|a| a.console.take_input())
    });
    if let Some(line) = line {
        execute_console_command(&line);
    }
}

// --- console command execution -------------------------------------------

/// Sane upper bounds the `give` command clamps to, mirroring Quake's pickup
/// caps (the player can't carry more than these).
const MAX_HEALTH: f32 = 250.0;
const MAX_ARMOR: f32 = 200.0;
const MAX_SHELLS: f32 = 100.0;
const MAX_NAILS: f32 = 200.0;
const MAX_ROCKETS: f32 = 100.0;
const MAX_CELLS: f32 = 100.0;

// QuakeC `items` weapon bits (quakedef.h): the weapon for digit `d` (1..8) is
// IT_SHOTGUN << (d-1), and IT_AXE is a separate high bit.
const IT_SHOTGUN: i32 = 1; // bit for weapon 2 base; weapon d>=2 is IT_SHOTGUN<<(d-2)
const IT_AXE: i32 = 4096;
const FL_GODMODE: i32 = 64;
const MOVETYPE_WALK: f32 = 3.0;
const MOVETYPE_FLY: f32 = 5.0;
const MOVETYPE_NOCLIP: f32 = 8.0;

/// Parse `line` into whitespace argv and run the matching console command
/// against the live [`Walk`], appending any output to the console scrollback.
/// An empty line does nothing; an unknown command prints
/// `"unknown command: <cmd>"`. Commands that touch the player edict guard on a
/// live walk and print `"no active game"` when there is none. Nothing here
/// panics on a bad/missing argument (all parsing uses `.ok()`/defaults).
fn execute_console_command(line: &str) {
    let argv: Vec<&str> = line.split_whitespace().collect();
    let Some(&cmd) = argv.first() else { return };
    let cmd_lower = cmd.to_ascii_lowercase();

    // Commands that don't need the walk: echo / clear / help / cmdlist.
    match cmd_lower.as_str() {
        "clear" => {
            ensure_app(|a| a.console.clear());
            return;
        }
        "echo" => {
            let text = if argv.len() > 1 {
                argv[1..].join(" ")
            } else {
                String::new()
            };
            ensure_app(|a| a.console.println(text));
            return;
        }
        "help" | "cmdlist" => {
            ensure_app(|a| {
                a.console.println("commands:");
                a.console.println("  god noclip fly kill");
                a.console.println("  give <h|a|s|n|r|c|1-8> [n]");
                a.console.println("  impulse <n>   map <name>");
                a.console.println("  echo <text>   clear   help");
            });
            return;
        }
        _ => {}
    }

    // `map <name>` rebuilds the walk on a new level; handle it specially because
    // it replaces the whole Walk (can't be done while holding a &mut to it).
    if cmd_lower == "map" {
        run_map_command(argv.get(1).copied());
        return;
    }

    // The remaining commands act on the live player edict. Run them under a
    // single borrow; guard a missing walk with "no active game".
    ensure_app(|a| {
        let has_walk = a.walk.is_some();
        if !has_walk {
            a.console.println("no active game");
            return;
        }
        // Split the borrow: the walk (player edict + vm) and the console output.
        // Take the player index + a raw pointer-free reference via the App.
        let mut out: Vec<String> = Vec::new();
        if let Some(w) = a.walk.as_mut() {
            run_game_command(w, &cmd_lower, &argv, &mut out);
        }
        for line in out {
            a.console.println(line);
        }
    });

    // An unrecognised command: report it. (Handled here so the borrow above can
    // finish first; run_game_command pushes nothing for an unknown verb.)
    let known = matches!(
        cmd_lower.as_str(),
        "god" | "noclip" | "fly" | "kill" | "give" | "impulse"
    );
    if !known {
        ensure_app(|a| a.console.println(format!("unknown command: {cmd}")));
    }
}

/// Run a command that acts on the live player edict, pushing any output lines
/// into `out`. `cmd` is already lowercased; `argv[0]` is the command itself.
/// Unknown verbs push nothing (the caller reports them).
fn run_game_command(w: &mut Walk, cmd: &str, argv: &[&str], out: &mut Vec<String>) {
    let player = w.player;
    match cmd {
        // Host_God_f: flags ^= FL_GODMODE.
        "god" => {
            let flags = w.server.vm.ent_get_float(player, "flags") as i32 ^ FL_GODMODE;
            w.server.vm.ent_set_float(player, "flags", flags as f32);
            out.push(if flags & FL_GODMODE != 0 {
                "godmode ON".into()
            } else {
                "godmode OFF".into()
            });
        }
        // Host_Noclip_f: movetype toggles WALK <-> NOCLIP.
        "noclip" => {
            let mt = w.server.vm.ent_get_float(player, "movetype");
            if mt != MOVETYPE_NOCLIP {
                w.server.vm.ent_set_float(player, "movetype", MOVETYPE_NOCLIP);
                out.push("noclip ON".into());
            } else {
                w.server.vm.ent_set_float(player, "movetype", MOVETYPE_WALK);
                out.push("noclip OFF".into());
            }
        }
        // Host_Fly_f: movetype toggles WALK <-> FLY.
        "fly" => {
            let mt = w.server.vm.ent_get_float(player, "movetype");
            if mt != MOVETYPE_FLY {
                w.server.vm.ent_set_float(player, "movetype", MOVETYPE_FLY);
                out.push("flymode ON".into());
            } else {
                w.server.vm.ent_set_float(player, "movetype", MOVETYPE_WALK);
                out.push("flymode OFF".into());
            }
        }
        // Set health to 0 — the player dies on its next think.
        "kill" => {
            w.server.vm.ent_set_float(player, "health", 0.0);
            out.push("ouch!".into());
        }
        // Queue a one-shot impulse (impulse 9 = the QuakeC give-all cheat).
        "impulse" => {
            let n = argv.get(1).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
            w.next_impulse = n;
            out.push(format!("impulse {n}"));
        }
        // Host_Give_f (faithful-ish): a letter/digit selects what to give.
        "give" => run_give_command(w, argv, out),
        _ => {}
    }
}

/// Port of `Host_Give_f` for the single-player items we support: a letter
/// argument grants ammo/health/armour; a digit 1..8 grants that weapon (setting
/// the `items` bit and selecting it). `n` (argv[2]) is the amount; ammo/health
/// default to a full amount when omitted. All values clamp to sane caps.
fn run_give_command(w: &mut Walk, argv: &[&str], out: &mut Vec<String>) {
    let what = match argv.get(1) {
        Some(s) if !s.is_empty() => *s,
        _ => {
            out.push("give what? (h a s n r c 1-8)".into());
            return;
        }
    };
    let c0 = what.as_bytes()[0] as char;
    let player = w.player;
    // The amount (argv[2]); None => use the per-field default below.
    let amount = argv.get(2).and_then(|s| s.parse::<f32>().ok());

    let mut set = |field: &str, val: f32, cap: f32, label: &str, out: &mut Vec<String>| {
        let v = val.clamp(0.0, cap);
        w.server.vm.ent_set_float(player, field, v);
        out.push(format!("gave {label} {v:.0}"));
    };

    match c0 {
        'h' => set("health", amount.unwrap_or(MAX_HEALTH), MAX_HEALTH, "health", out),
        'a' => set("armorvalue", amount.unwrap_or(MAX_ARMOR), MAX_ARMOR, "armor", out),
        's' => set("ammo_shells", amount.unwrap_or(MAX_SHELLS), MAX_SHELLS, "shells", out),
        'n' => set("ammo_nails", amount.unwrap_or(MAX_NAILS), MAX_NAILS, "nails", out),
        'r' => set("ammo_rockets", amount.unwrap_or(MAX_ROCKETS), MAX_ROCKETS, "rockets", out),
        'c' => set("ammo_cells", amount.unwrap_or(MAX_CELLS), MAX_CELLS, "cells", out),
        '1'..='8' => {
            // Weapon select/grant. Weapon 1 = axe (its own high bit); weapons
            // 2..8 are IT_SHOTGUN << (d-2), exactly as Host_Give_f does
            // (sv_player->v.items |= IT_SHOTGUN << (t[0]-'2')).
            let d = (c0 as u8 - b'0') as i32; // 1..8
            let bit = if d == 1 {
                IT_AXE
            } else {
                IT_SHOTGUN << (d - 2)
            };
            let items = w.server.vm.ent_get_float(player, "items") as i32 | bit;
            w.server.vm.ent_set_float(player, "items", items as f32);
            // Select it: the QuakeC `weapon` field is the active weapon bit.
            w.server.vm.ent_set_float(player, "weapon", bit as f32);
            out.push(format!("gave weapon {d}"));
        }
        _ => out.push(format!("give: unknown item '{what}'")),
    }
}

/// Run `map <name>`: build a fresh walk on `maps/<name>.bsp`. On success swap the
/// walk, close the console, and print `"loading <name>"`; on failure print
/// `"map not found: <name>"` and keep the current level.
fn run_map_command(name: Option<&str>) {
    let Some(name) = name.filter(|s| !s.is_empty()) else {
        ensure_app(|a| a.console.println("usage: map <name>"));
        return;
    };
    let path = format!("maps/{name}.bsp");
    // Build the new walk OUTSIDE the borrow (it reads the pak + parses a BSP).
    let new_walk = build_walk_map(&path);
    ensure_app(|a| match new_walk {
        Some(nw) => {
            a.walk = Some(nw);
            a.mode = 0;
            a.console.println(format!("loading {name}"));
            // The level loaded: close the console so the player sees the new map.
            a.console.open = false;
            // Keep the menu closed too (a `map` from the console starts play).
            a.menu = Menu::new();
            // Preserve the player's chosen render resolution across a `map` (the
            // C keeps the video mode); the dispatcher re-syncs the Options label
            // to the live render size, so the fresh menu's preset can't desync.
        }
        None => a.console.println(format!("map not found: {name}")),
    });
}

/// Clamp the view pitch the way `CL_AdjustAngles` (cl_input.c) does: pitch is
/// limited to `[-70, 80]`. In this codebase positive pitch = looking down
/// (UserCmd pitch is QuakeC's +down convention), so +80 is the further-down
/// bound and -70 the looking-up bound — an asymmetry matching Quake's feel.
fn clamp_pitch(pitch: f32) -> f32 {
    pitch.clamp(-70.0, 80.0)
}

#[no_mangle]
pub extern "C" fn look(dyaw: f32, dpitch: f32) {
    ensure_app(|a| {
        // While the menu OR console is up, Quake freezes the view (key_dest !=
        // key_game stops feeding mouse-look). Match that: ignore look input
        // behind either overlay so the idle world doesn't rotate underneath it.
        if a.menu.visible || a.console.open {
            return;
        }
        if let Some(w) = a.walk.as_mut() {
            w.yaw += dyaw;
            w.pitch = clamp_pitch(w.pitch + dpitch);
        }
    });
}

/// Advance the active mode by `dt` seconds and render into the framebuffer.
#[no_mangle]
pub extern "C" fn step(dt: f32) {
    ensure_app(|a| {
        // Advance the App clock (drives the menu cursor animation; mode-independent
        // so the cursor keeps blinking over a frozen frame). Guard a non-finite /
        // negative dt so it only moves forward.
        if dt.is_finite() && dt > 0.0 {
            a.clock += dt;
        }
        let (w, h) = (a.render_w, a.render_h);
        // While the menu OR console is up, gameplay input is gated; the dispatcher
        // owns that state, so it tells step_walk whether to gate. step_demo ignores
        // gameplay input regardless. The console takes priority over the menu.
        let menu_visible = a.menu.visible;
        let gate_gameplay = menu_visible || a.console.open;
        let mut img = if a.mode == 1 {
            a.demo.as_mut().map(|d| step_demo(d, dt, w, h))
        } else {
            a.walk.as_mut().map(|wk| step_walk(wk, dt, gate_gameplay, w, h))
        };

        // The main menu overlays WHATEVER is playing (walk OR the attract demo).
        // Drawn here in the dispatcher, after the active mode rendered its frame
        // and BEFORE packing to the framebuffer, so it sits on top of everything.
        // Uses the active mode's palette and the App clock for the cursor frame.
        if menu_visible {
            // Keep the Options "Screen size" label tracking the actual render
            // resolution (the framebuffer is the source of truth), so a boot /
            // New Game / `map` that changed the render size can't leave the label
            // stale.
            a.menu.sync_resolution(a.render_w as i32, a.render_h as i32);
            if let Some(img) = img.as_mut() {
                if let Some(palette) = a.active_palette() {
                    render::draw_menu(
                        img,
                        &a.menu,
                        &a.menu_pics,
                        a.conchars.as_ref(),
                        a.clock,
                        palette,
                    );
                }
            }
        }

        // The console overlays EVERYTHING — drawn last (after the menu), so the
        // dropped-down panel sits on top of the menu too. It owns the keyboard
        // while open. Uses the active mode's palette and the App clock for the
        // input cursor blink. A closed console draws nothing.
        if a.console.open {
            if let Some(img) = img.as_mut() {
                if let Some(palette) = a.active_palette() {
                    render::draw_console(
                        img,
                        &a.console,
                        a.conback.as_ref(),
                        a.conchars.as_ref(),
                        palette,
                        a.clock,
                    );
                }
            }
        }

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
    /// True when this sound came from the listener's own view entity (the
    /// player edict). The C `SND_Spatialize` (snd_dma.c:407-412) forces such
    /// sounds to full master volume on both channels with NO distance falloff
    /// or pan; the page reads this via `sound_is_view_entity` to skip its
    /// spatial attenuation for player-local sounds (weapon fire, pain, etc.).
    is_view_entity: bool,
}

impl SndParams {
    const fn zero() -> Self {
        SndParams {
            origin: [0.0; 3],
            volume: 0.0,
            attenuation: 0.0,
            is_view_entity: false,
        }
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
    /// Whether the page's `AudioContext` is running yet. The page calls
    /// `set_audio_ready(1)` once the context resumes (it starts suspended until a
    /// user gesture). Until then `queue_sounds` drops sounds on the floor instead
    /// of appending them every frame up to the 12-cap — otherwise a backlog of
    /// stale sounds from before audio started would all play at once when it does.
    static AUDIO_READY: RefCell<bool> = const { RefCell::new(false) };
}

/// Page hook (LOW-10): set once the browser `AudioContext` has resumed to the
/// `running` state. While this is `0` the per-frame sound queue is not filled,
/// so no pre-audio backlog accumulates to flush when playback finally starts.
#[no_mangle]
pub extern "C" fn set_audio_ready(ready: i32) {
    AUDIO_READY.with(|r| *r.borrow_mut() = ready != 0);
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

/// Load the WAV bytes for the gameplay sounds in `events` and push them onto the
/// playback queue. Constant ambient loops (`ambience/*`) and the silent
/// `misc/null.wav` are skipped, and the queue is capped at 12 so a noisy frame
/// can't grow it without bound.
///
/// Mixing follows the C `SND_PickChannel` (snd_dma.c:354-390), keyed on the
/// `(entity, channel)` pair carried by each `SoundEvent` — NOT on the sample
/// name. A repeat `(entity, channel)` with a non-zero channel RESTARTS that
/// channel (the later event overrides the earlier queued one for that key,
/// matching "always override sound from same entity"); `channel == -1` matches
/// any channel of that entity. Channel 0 NEVER overrides (the C comment:
/// "channel 0 never overrides") so every channel-0 emitter queues separately.
/// This keeps two distinct emitters of the SAME sample (e.g. two doors, or a
/// gunshot and a footstep) from collapsing into one and losing the other's
/// origin/volume.
///
/// `view_entity` is the listener's own edict (the player): a sound from it is
/// flagged so the page plays it at full volume with no falloff (see
/// `SndParams::is_view_entity`).
fn queue_sounds(pak: &Pak, events: &[quake_rs::server::SoundEvent], view_entity: i32) {
    if events.is_empty() {
        return;
    }
    // LOW-10: don't accumulate a backlog before audio starts. `drainGameSounds`
    // early-returns while the AudioContext is suspended, but the queue would
    // keep growing to its cap every frame and then dump stale sounds the moment
    // audio resumes. Skip enqueuing entirely until the page reports the context
    // running via `set_audio_ready(1)`.
    if !AUDIO_READY.with(|r| *r.borrow()) {
        return;
    }
    // Queue indices of entries already placed this call, keyed by their
    // (entity, channel) — only for non-zero channels (channel 0 never
    // overrides, so it is never recorded here and always appends).
    let mut placed: Vec<((i32, i32), usize)> = Vec::new();
    SND_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        for ev in events {
            let name = ev.sample.as_str();
            if name.is_empty()
                || name == "misc/null.wav"
                || name.starts_with("ambience/")
            {
                continue;
            }

            // QuakeC sample names are relative to the "sound/" directory (the C
            // `S_LoadSound` does sprintf(buf, "sound/%s", name)); the pak stores
            // them under that prefix. Without it every read_file misses and the
            // sound is silently dropped — the long-standing "no in-game sound".
            let path = format!("sound/{name}");

            let params = SndParams {
                origin: ev.origin,
                volume: ev.volume,
                attenuation: ev.attenuation,
                is_view_entity: ev.entity == view_entity,
            };

            // Channel restart (SND_PickChannel): a non-zero channel from the
            // same entity overrides that entity's prior queued entry on the same
            // channel, so the channel plays the latest sound, not a stale one.
            // The C wildcard is `entchannel == -1` on the NEW event only; QuakeC's
            // SV_StartSound emit path always carries a concrete channel 0..7
            // (the -1 "any" form is only used to STOP sounds, never emitted here),
            // so we match the C's predicate exactly: new-channel -1 is a wildcard.
            if ev.channel != 0 {
                let hit = placed.iter().position(|&((e, c), _)| {
                    e == ev.entity && (c == ev.channel || ev.channel == -1)
                });
                if let Some(pi) = hit {
                    let qi = placed[pi].1;
                    if let Ok(Some(bytes)) = pak.read_file(&path) {
                        q[qi] = (bytes, params);
                        // Re-key to this channel so a following -1 still matches.
                        placed[pi].0 = (ev.entity, ev.channel);
                    }
                    continue;
                }
            }

            if q.len() >= 12 {
                continue; // queue cap reached
            }
            if let Ok(Some(bytes)) = pak.read_file(&path) {
                let qi = q.len();
                q.push((bytes, params));
                if ev.channel != 0 {
                    placed.push(((ev.entity, ev.channel), qi));
                }
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

/// `1` when the entry the most recent `poll_sound` popped came from the
/// listener's own view entity (the player edict), else `0`. The C
/// `SND_Spatialize` (snd_dma.c:407-412) forces view-entity sounds to full
/// master volume on both channels with no distance falloff or pan; the page
/// reads this to take the same full-volume / centred path for the player's own
/// sounds (weapon fire, pain) instead of attenuating them with distance.
#[no_mangle]
pub extern "C" fn sound_is_view_entity() -> i32 {
    SND_CUR.with(|p| p.borrow().is_view_entity as i32)
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

/// Quake alias-model header flags (the mdl `flags` field, distinct from an
/// entity's `effects`): the projectile/gib trail bits CL_RelinkEntities reads to
/// spawn R_RocketTrail behind a moving model.
const MF_ROCKET: i32 = 1;
const MF_GRENADE: i32 = 2;
const MF_GIB: i32 = 4;
const MF_TRACER: i32 = 16;
const MF_ZOMGIB: i32 = 32;
const MF_TRACER2: i32 = 64;
const MF_TRACER3: i32 = 128;

/// Map a model's header flags to its R_RocketTrail type (0 rocket, 1 grenade,
/// 2 gib-blood, 3 tracer, 4 zombie-gib, 5 tracer2, 6 voor), or `None` if the
/// model leaves no trail. Order matches CL_RelinkEntities' if/else chain.
fn rocket_trail_type(model_flags: i32) -> Option<i32> {
    if model_flags & MF_ROCKET != 0 {
        Some(0)
    } else if model_flags & MF_GRENADE != 0 {
        Some(1)
    } else if model_flags & MF_GIB != 0 {
        Some(2)
    } else if model_flags & MF_ZOMGIB != 0 {
        Some(4)
    } else if model_flags & MF_TRACER != 0 {
        Some(3)
    } else if model_flags & MF_TRACER2 != 0 {
        Some(5)
    } else if model_flags & MF_TRACER3 != 0 {
        Some(6)
    } else {
        None
    }
}

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
        // Rocket explosion: R_ParticleExplosion + r_exp3 (CL_ParseTEnt).
        TE_EXPLOSION => {
            particles.spawn_explosion(ev.pos, now, rng);
            Some(TE_EXPLOSION_SOUND)
        }
        // Tar/blob explosion (Scrag/Vore): R_BlobExplosion — distinct two-ramp
        // effect, NOT the rocket explosion (the dlight is also dropped below).
        TE_TAREXPLOSION => {
            particles.spawn_blob_explosion(ev.pos, now, rng);
            Some(TE_EXPLOSION_SOUND)
        }
        // Coloured explosion: R_ParticleExplosion2 honouring the colour ramp
        // (color_start/color_length carried on the temp-entity event).
        TE_EXPLOSION2 => {
            particles.spawn_explosion2(
                ev.pos,
                ev.color_start as i32,
                ev.color_length as i32,
                now,
                rng,
            );
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
        // The real lava-burst spiral (R_LavaSplash), not a 20-particle puff.
        TE_LAVASPLASH => {
            particles.spawn_lava_splash(ev.pos, now, rng);
            None
        }
        // The teleport-fog column (R_TeleportSplash).
        TE_TELEPORT => {
            particles.spawn_teleport_splash(ev.pos, now, rng);
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
    // Capture serverflags (the episode rune SERVERFLAG_* bits) from the OUTGOING
    // server. The C keeps these alive across SV_SpawnServer (svs.serverflags);
    // building a brand-new Server would reset the global to 0 and lose the
    // collected runes, so we carry it forward onto the new level below.
    let serverflags = w.server.serverflags();
    // Carry the chosen difficulty across the level change. `skill` is a
    // thread-local that Server::with_pak resets to 1, so capture it from the
    // OUTGOING server now and restore it on the new one below (the start hub's
    // skill portal set it via cvar_set; without this the jump to e1m1 would
    // silently revert to Normal). Mirrors how serverflags is carried.
    let skill = w.server.skill();

    let read = |n: &str| w.pak.read_file(n).ok().flatten();
    // The QuakeC `changelevel(map)` carries the BARE map name (e.g. "e1m1", from
    // the trigger's `map` key), but the pak stores it as "maps/e1m1.bsp". Build
    // the pak path (tolerating an already-qualified name). Without this the read
    // missed and the swap silently aborted — the start-hub episode-1 slipgate
    // (and every in-game changelevel) "did nothing".
    let map_file = if next_map.ends_with(".bsp") {
        next_map.to_string()
    } else {
        format!("maps/{next_map}.bsp")
    };
    // Two BSP copies (one for the sim/collision world the server owns, one for
    // rendering) plus a fresh progs.dat for the new server. Any failure aborts
    // the swap, leaving the live level running.
    let Some(map_bytes) = read(&map_file) else { return };
    let Ok(sim_bsp) = Bsp::parse(&map_bytes) else { return };
    let Ok(render_bsp) = Bsp::parse(&map_bytes) else { return };
    let Some(progs_bytes) = read("progs.dat") else { return };
    let Ok(progs) = Progs::parse(&progs_bytes) else { return };

    let Ok(mut ns) = Server::with_pak(sim_bsp, progs, Some(w.pak.clone())) else { return };
    // Restore the carried serverflags onto the new server BEFORE spawning its
    // entities, mirroring the C (SV_SpawnServer restores svs.serverflags before
    // ED_LoadFromFile), so the new level's worldspawn — which reads serverflags
    // to light up the runes the player already holds — and the reconnecting
    // client both observe the carried bits. A no-op if the progs lacks the
    // global.
    ns.set_serverflags(serverflags);
    ns.set_skill(skill as f32);
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
    w.trail_org.clear();
    w.clock = 0.0;
    // Reset the screen-blend state so the level change does not flash red.
    w.damage_blend = 0.0;
    w.last_total = f32::NAN;
    // Drop any events the *outgoing* server queued (the new server starts fresh).
    let _ = w.server.drain_sounds();
    let _ = w.server.drain_particles();
    let _ = w.server.drain_temp_entities();
}

fn step_walk(
    w: &mut Walk,
    dt: f32,
    menu_up: bool,
    render_w: usize,
    render_h: usize,
) -> render::Image {
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
    let (mut fwd, mut side) = if menu_up { (0.0, 0.0) } else { (w.in_fwd, w.in_side) };
    let mag = (fwd * fwd + side * side).sqrt();
    if mag > 1.0 {
        fwd /= mag;
        side /= mag;
    }
    let cmd = UserCmd {
        forwardmove: fwd * SPEED,
        sidemove: side * SPEED,
        // Vertical swim intent: Space (jump) = up, c (movedown) = down. Quake's
        // SV_WaterMove consumes upmove while waist-deep; the ground/air move
        // ignores it, so on land Space still just jumps and c does nothing.
        upmove: if menu_up {
            0.0
        } else {
            ((if w.in_jump { 1.0 } else { 0.0 }) - (if w.in_down { 1.0 } else { 0.0 })) * SPEED
        },
        yaw: w.yaw,
        pitch: w.pitch,
        buttons: if menu_up {
            0
        } else {
            (if w.in_attack { 1 } else { 0 }) | (if w.in_jump { 2 } else { 0 })
        },
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
    queue_sounds(&w.pak, &events, w.player);

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
            // Only TE_EXPLOSION and TE_EXPLOSION2 flash a dynamic light in id's
            // CL_ParseTEnt; TE_TAREXPLOSION (blob) does NOT.
            if matches!(ev.te_type, TE_EXPLOSION | TE_EXPLOSION2) {
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
        // Temp-entity sounds (explosions, wall impacts) carry entity=0,
        // channel=0 -> never the view entity, never channel-restarted, so each
        // distinct explosion queues separately at its own origin.
        queue_sounds(&w.pak, &te_sounds, w.player);
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
        } else if m.ends_with(".bsp") && m != w.map_name && !w.bmodel_cache.contains_key(&m) {
            // An external brush-model item box (maps/b_*.bsp). Parse once and cache;
            // a missing/unparseable box stores `None` so we never re-read or panic.
            let parsed = w.pak.read_file(&m).ok().flatten().and_then(|b| Bsp::parse(&b).ok());
            w.bmodel_cache.insert(m, parsed);
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
    // Projectile/gib trails to spawn this frame, collected here and emitted after
    // the loop (so we don't borrow w.particles/dlights while reading the server):
    // (entity, old origin, new origin, R_RocketTrail type).
    let mut trail_spawns: Vec<(i32, [f32; 3], [f32; 3], i32)> = Vec::new();
    // External brush-model items (maps/b_*.bsp) as owned (name, origin) pairs; the
    // borrowing `ExternalBModel` list is built below, after the cache is final, so
    // the immutable cache borrow does not clash with reading the server here.
    let mut ext_descs: Vec<(String, [f32; 3])> = Vec::new();
    for e in 0..n {
        let ent = e as i32;
        if ent == w.player || w.server.vm.edict_free.get(e).copied().unwrap_or(true) {
            continue;
        }
        // Render an entity only when it has a real modelindex — i.e. its QuakeC
        // spawn actually called setmodel. An edict that early-returns before
        // setmodel (e.g. func_episodegate in shareware: serverflags=0 so the gate
        // stays passable) keeps its raw map `model` key like "*41" but never gets
        // a modelindex, and Quake leaves it invisible. Without this guard those
        // gates draw as phantom walls the player walks through — and mask the real
        // slipgate behind them, so episode/level selection *looks* broken.
        if w.server.vm.ent_get_float(ent, "modelindex") == 0.0 {
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
        // External brush-model item boxes: a standalone b_*.bsp the item set as its
        // model (explosive box, ammo/health boxes). Not the world map itself.
        if m.ends_with(".bsp") {
            if m != w.map_name {
                let origin = w.server.vm.ent_get_vector(ent, "origin");
                ext_descs.push((m, origin));
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
        // R_RocketTrail: a model with a rocket/grenade/gib/tracer header flag
        // trails particles from its previous origin to here (CL_RelinkEntities).
        let mflags = w
            .model_cache
            .get(&m)
            .and_then(|o| o.as_ref())
            .map(|md| md.header.flags)
            .unwrap_or(0);
        if let Some(ttype) = rocket_trail_type(mflags) {
            let oldorg = *w.trail_org.get(&ent).unwrap_or(&origin);
            trail_spawns.push((ent, oldorg, origin, ttype));
            w.trail_org.insert(ent, origin);
        }
        descs.push((m, origin, yaw, frame, color));
    }

    // Emit the collected trails (after the entity loop to keep the borrows
    // disjoint). spawn_rocket_trail steps from old->new origin; EF_ROCKET also
    // flashes a small dynamic light at the rocket head.
    for (ent, oldorg, neworg, ttype) in trail_spawns.drain(..) {
        w.particles
            .spawn_rocket_trail(oldorg, neworg, ttype, &mut w.tracercount, now, &mut w.prng);
        if ttype == 0 {
            w.dlights.alloc(ent, neworg, 200.0, now + 0.01, 0.0, 0.0, now);
        }
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
                skinnum: 0,
            }),
            _ => None,
        })
        .collect();
    // External brush-model item boxes: resolve each (name, origin) against the
    // bmodel cache, dropping any box whose bsp was missing/unparseable (`None`).
    let external: Vec<render::ExternalBModel> = ext_descs
        .iter()
        .filter_map(|(name, origin)| match w.bmodel_cache.get(name) {
            Some(Some(bsp)) => Some(render::ExternalBModel { bsp, origin: *origin }),
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
        render::render_scene_ext(&w.bsp, &cam, render_w, render_h, &w.palette, &instances, &bmodels, &external, viewmodel, w.clock, &parts, &active_dlights, &light_styles, w.colormap.as_deref());

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
            // The active weapon's ammo (W_SetCurrentAmmo keeps `currentammo` in
            // sync with the weapon), not always shells — sbar.c draws currentammo.
            ammo: stat("currentammo"),
            armor: stat("armorvalue"),
            items: stat("items"),
            weapon: stat("weapon"),
            ammo_shells: stat("ammo_shells"),
            ammo_nails: stat("ammo_nails"),
            ammo_rockets: stat("ammo_rockets"),
            ammo_cells: stat("ammo_cells"),
            time: w.clock,
        };
        render::draw_hud_into(&mut img, &hud);
    }

    // The main-menu overlay is drawn by the `step` dispatcher (the menu lives at
    // the App level now so it can overlay walk OR the attract demo); step_walk no
    // longer draws it.
    img
}

/// Spawn the recorded effects of demo frame `idx` into the live particle pool
/// exactly ONCE: a frame rendered across several steps (small `dt`) must not
/// re-spawn its bursts each step. `d.last_spawned_idx` records the most recently
/// spawned frame; this is a no-op when it already equals `idx`.
///
/// Each `svc_particle` burst replays through [`ParticleSystem::spawn_burst`],
/// except the explosion sentinel (`count >= 1024`, the demo parser's mapping of
/// the net `count == 255`) which routes to [`ParticleSystem::spawn_explosion`]
/// for the 1024-particle fiery burst. Each temp entity replays through the same
/// [`spawn_temp_entity`] mapping the live walk uses (explosion / impact / splash).
/// The frame's recorded server `time` is the absolute clock for particle
/// lifetimes (`spawn_*` set `die = now + life`).
fn spawn_demo_frame_effects(d: &mut DemoPlay, idx: usize) {
    if d.last_spawned_idx == idx {
        return; // already spawned this frame's effects; don't double-spawn
    }
    d.last_spawned_idx = idx;
    let Some(frame) = d.demo.frames.get(idx) else { return };
    let now = frame.time;
    // The frame borrows `d.demo`; copy the small effect records out so we can
    // call &mut self spawn methods on `d.particles` without aliasing `d`.
    let bursts = frame.particles.clone();
    let tents = frame.temp_entities.clone();
    for b in &bursts {
        // svc_particle is always R_RunParticleEffect (spawn_burst) in id's
        // CL_ParseParticleEffect — the net count==255 sentinel just means 1024
        // particles (the demo parser already maps it), NOT the rocket
        // R_ParticleExplosion. Route every burst through spawn_burst.
        d.particles
            .spawn_burst(b.org, b.dir, b.color, b.count, now, &mut d.prng);
    }
    for ev in &tents {
        // Reuse the live-walk mapping (explosion/impact/splash). The returned
        // sound is the explosion SFX; demo playback drives audio through its own
        // svc_sound stream, so we ignore it here (the visual effect is the point).
        let _ = spawn_temp_entity(&mut d.particles, ev, now, &mut d.prng);
    }
}

fn step_demo(d: &mut DemoPlay, dt: f32, render_w: usize, render_h: usize) -> render::Image {
    let n = d.demo.frames.len();
    let t0 = d.demo.frames[0].time;
    d.elapsed += dt;
    // Wrap BEFORE advancing: only loop back to frame 0 once we were already
    // sitting on the last frame on a prior step and time has run past it. This
    // defers the reset by one step so frames[n-1] is rendered (displayed for its
    // dt) before we snap back to the start — the previous code reset to 0 the
    // instant `idx` reached n-1, so the final frame was never shown.
    if d.idx + 1 >= n {
        d.idx = 0;
        d.elapsed = 0.0;
        // Looping restarts the recorded effect stream: drop every live particle
        // and forget what was spawned so the replay from frame 0 is identical to
        // the first pass (no stale explosions carried across the wrap).
        d.particles = ParticleSystem::new();
        d.last_spawned_idx = usize::MAX;
    }
    // Advance to the frame matching the recorded server time. Stop at the last
    // frame (n-1); the wrap above handles looping on the FOLLOWING step. Spawn
    // the recorded effects of EACH frame we newly advance onto (a large dt can
    // step over several frames at once; missing one would drop its explosion).
    while d.idx + 1 < n && (d.demo.frames[d.idx + 1].time - t0) <= d.elapsed {
        d.idx += 1;
        spawn_demo_frame_effects(d, d.idx);
    }
    // Also spawn the landing frame's effects when we first arrive on it without
    // the while-loop running (e.g. the very first step lands on frame 0, or a
    // tiny dt holds us on the same frame the wrap reset us to). `last_spawned_idx`
    // guards against re-spawning while a frame lingers across several steps.
    spawn_demo_frame_effects(d, d.idx);

    // Age the live particle pool one frame under the same gentle gravity the
    // live walk uses (sv_gravity * 0.05 with the default sv_gravity = 800), then
    // retire the expired ones. Guarded against a non-finite/negative dt.
    if dt.is_finite() && dt > 0.0 {
        let now = d.demo.frames[d.idx].time;
        d.particles.advance(dt, now, 800.0 * 0.05);
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
                skinnum: 0,
            });
        }
    }
    let cam = Camera {
        pos: f.view_origin,
        yaw: f.view_angles[1],
        pitch: -f.view_angles[0],
        fov_deg: 90.0,
    };
    // The recorded server time animates the demo's liquids/sky too. The live
    // particle pool (replayed from the recorded svc_particle / temp-entity
    // stream) is passed as (world pos, palette index) so blood/puffs/explosions
    // draw into the scene sharing its z-buffer. Demos carry no dynamic lights
    // here (empty) and no live server for light styles (neutral static scales).
    let parts: Vec<([f32; 3], u8)> =
        d.particles.particles().iter().map(|p| (p.origin, p.color)).collect();
    render::render_scene_ext(&d.bsp, &cam, render_w, render_h, &d.palette, &owned, &[], &[], None, f.time, &parts, &[], &render::NEUTRAL_LIGHTSTYLE_SCALES, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitch_clamp_is_asymmetric_like_cl_adjustangles() {
        // CL_AdjustAngles clamps pitch to [-70, 80]; positive pitch = down.
        assert_eq!(clamp_pitch(0.0), 0.0, "neutral pitch is unchanged");
        // Looking far down is allowed up to +80, not +70.
        assert_eq!(clamp_pitch(200.0), 80.0, "down clamps at +80");
        assert_eq!(clamp_pitch(75.0), 75.0, "75 down is within the +80 bound");
        assert_eq!(clamp_pitch(80.0), 80.0, "exactly +80 is allowed");
        // Looking up is limited to -70.
        assert_eq!(clamp_pitch(-200.0), -70.0, "up clamps at -70");
        assert_eq!(clamp_pitch(-70.0), -70.0, "exactly -70 is allowed");
        // The asymmetry: +75 survives but -75 is clamped to -70.
        assert!(
            clamp_pitch(75.0) > 70.0 && clamp_pitch(-75.0) == -70.0,
            "down range exceeds 70 while up range does not"
        );
    }

    use quake_rs::pak::{Pak, DIRENTRY_SIZE, HEADER_SIZE, NAME_SIZE};
    use quake_rs::server::SoundEvent;

    /// Build a synthetic PACK image holding the given (name, contents) files, so
    /// `queue_sounds` can resolve real bytes for hand-crafted sound names without
    /// depending on the embedded pak's contents.
    fn build_test_pak(files: &[(&str, &[u8])]) -> Pak {
        let mut contents = Vec::new();
        let mut positions = Vec::new();
        let mut cursor = HEADER_SIZE as i32;
        for (_, data) in files {
            positions.push((cursor, data.len() as i32));
            contents.extend_from_slice(data);
            cursor += data.len() as i32;
        }
        let dirofs = HEADER_SIZE + contents.len();
        let dirlen = files.len() * DIRENTRY_SIZE;

        let mut img = Vec::new();
        img.extend_from_slice(b"PACK");
        img.extend_from_slice(&(dirofs as i32).to_le_bytes());
        img.extend_from_slice(&(dirlen as i32).to_le_bytes());
        img.extend_from_slice(&contents);
        for (i, (name, _)) in files.iter().enumerate() {
            let mut name_field = [0u8; NAME_SIZE];
            let b = name.as_bytes();
            name_field[..b.len()].copy_from_slice(b);
            img.extend_from_slice(&name_field);
            img.extend_from_slice(&positions[i].0.to_le_bytes());
            img.extend_from_slice(&positions[i].1.to_le_bytes());
        }
        Pak::from_bytes("test".into(), img).expect("synthetic pak")
    }

    fn ev(entity: i32, channel: i32, sample: &str, vol: f32) -> SoundEvent {
        SoundEvent {
            entity,
            channel,
            sound_index: -1,
            sample: sample.to_string(),
            origin: [vol, 0.0, 0.0], // stash a tag in origin.x so we can identify it
            volume: vol,
            attenuation: 1.0,
        }
    }

    /// Drain the whole queue into a list of (volume, is_view_entity) via the same
    /// poll path the page uses.
    fn drain_queue() -> Vec<(f32, bool)> {
        let mut out = Vec::new();
        loop {
            let len = poll_sound();
            if len == 0 {
                break;
            }
            out.push((sound_volume(), sound_is_view_entity() != 0));
        }
        out
    }

    fn reset_queue() {
        SND_QUEUE.with(|q| q.borrow_mut().clear());
        set_audio_ready(1); // audio running so queue_sounds enqueues
    }

    #[test]
    fn queue_sounds_keys_by_entity_channel_not_sample_name() {
        let pak = build_test_pak(&[("sound/a.wav", b"AAAA"), ("sound/b.wav", b"BBBB")]);

        // Two DISTINCT emitters of the SAME sample (different entities, channel 0
        // each) must BOTH queue — the old by-name dedup would have dropped one.
        reset_queue();
        queue_sounds(
            &pak,
            &[ev(2, 0, "a.wav", 0.3), ev(5, 0, "a.wav", 0.7)],
            /*view_entity*/ -1,
        );
        let got = drain_queue();
        assert_eq!(got.len(), 2, "distinct emitters of the same sample both queue");
        assert_eq!(got[0].0, 0.3);
        assert_eq!(got[1].0, 0.7);

        // Same (entity, channel) with a NON-ZERO channel RESTARTS that channel:
        // the later event overrides the earlier queued entry (one slot, latest
        // params).
        reset_queue();
        queue_sounds(
            &pak,
            &[ev(2, 1, "a.wav", 0.2), ev(2, 1, "b.wav", 0.9)],
            -1,
        );
        let got = drain_queue();
        assert_eq!(got.len(), 1, "same (entity,channel>0) collapses to one slot");
        assert_eq!(got[0].0, 0.9, "the restart keeps the LATER sound's params");

        // Channel 0 NEVER overrides: the same entity firing twice on channel 0
        // queues twice (Quake's auto-channel allocates fresh each time).
        reset_queue();
        queue_sounds(&pak, &[ev(2, 0, "a.wav", 0.1), ev(2, 0, "a.wav", 0.4)], -1);
        let got = drain_queue();
        assert_eq!(got.len(), 2, "channel 0 never overrides; both queue");

        // Different non-zero channels of the SAME entity are independent.
        reset_queue();
        queue_sounds(&pak, &[ev(2, 1, "a.wav", 0.5), ev(2, 2, "b.wav", 0.6)], -1);
        let got = drain_queue();
        assert_eq!(got.len(), 2, "distinct channels of one entity stay separate");
    }

    #[test]
    fn queue_sounds_flags_view_entity() {
        let pak = build_test_pak(&[("sound/a.wav", b"AAAA"), ("sound/b.wav", b"BBBB")]);
        reset_queue();
        // entity 7 is the player (view entity); entity 3 is a monster.
        queue_sounds(&pak, &[ev(7, 0, "a.wav", 0.5), ev(3, 0, "b.wav", 0.5)], 7);
        let got = drain_queue();
        assert_eq!(got.len(), 2);
        assert!(got[0].1, "the view entity's sound is flagged");
        assert!(!got[1].1, "the monster's sound is not flagged");
    }

    #[test]
    fn queue_sounds_skips_until_audio_ready() {
        let pak = build_test_pak(&[("sound/a.wav", b"AAAA")]);
        SND_QUEUE.with(|q| q.borrow_mut().clear());
        set_audio_ready(0); // audio not running yet
        queue_sounds(&pak, &[ev(2, 0, "a.wav", 0.5)], -1);
        assert!(
            SND_QUEUE.with(|q| q.borrow().is_empty()),
            "no sounds accumulate before audio is ready"
        );
        // Once ready, the same call enqueues.
        set_audio_ready(1);
        queue_sounds(&pak, &[ev(2, 0, "a.wav", 0.5)], -1);
        assert_eq!(SND_QUEUE.with(|q| q.borrow().len()), 1);
        SND_QUEUE.with(|q| q.borrow_mut().clear());
        set_audio_ready(0); // restore default for other tests
    }

    #[test]
    fn queue_sounds_resolves_bare_sample_to_sound_dir() {
        // A QuakeC sample name is bare ("weapons/guncock.wav"); the pak stores it
        // under "sound/". queue_sounds must prepend "sound/" or the lookup misses
        // and the sound is dropped (the long-standing "no in-game sound" bug).
        let pak = build_test_pak(&[("sound/weapons/guncock.wav", b"GUNC")]);
        reset_queue();
        queue_sounds(&pak, &[ev(7, 1, "weapons/guncock.wav", 0.8)], -1);
        let got = drain_queue();
        assert_eq!(got.len(), 1, "bare sample name must resolve under sound/");
        assert_eq!(got[0].0, 0.8);
        // A name with NO matching pak entry (even under sound/) queues nothing.
        reset_queue();
        queue_sounds(&pak, &[ev(7, 1, "weapons/nope.wav", 0.5)], -1);
        assert!(drain_queue().is_empty(), "missing sample is dropped, no panic");
        SND_QUEUE.with(|q| q.borrow_mut().clear());
        set_audio_ready(0);
    }

    #[test]
    fn step_demo_shows_the_last_frame_before_looping() {
        // FIX-7: the wrap must be DEFERRED so frames[n-1] is rendered for one
        // step before looping back to frame 0. The old code reset to 0 the
        // instant `idx` reached n-1, so the final frame was never displayed.
        use quake_rs::demo::{Demo, DemoFrame};

        let frame = |t: f32| DemoFrame {
            time: t,
            view_origin: [0.0, 0.0, 0.0],
            view_angles: [0.0, 0.0, 0.0],
            entities: Vec::new(),
            particles: Vec::new(),
            temp_entities: Vec::new(),
        };
        let demo = Demo {
            level_name: "test".into(),
            // map_name() reads model_precache[1]; unused by step_demo's indexing.
            model_precache: vec![String::new(), "maps/test.bsp".into()],
            sound_precache: Vec::new(),
            // Three frames at t = 0, 1, 2.
            frames: vec![frame(0.0), frame(1.0), frame(2.0)],
        };
        let mut d = DemoPlay {
            bsp: render::demo_room(),
            palette: [[0u8; 3]; 256],
            demo,
            models: Vec::new(),
            colors: Vec::new(),
            elapsed: 0.0,
            idx: 0,
            particles: ParticleSystem::new(),
            prng: Lcg::new(1),
            last_spawned_idx: usize::MAX,
        };
        let n = d.demo.frames.len();

        // Drive several 1.0s steps and record which frame index is RENDERED
        // (i.e. the value of `idx` chosen by step_demo for that frame).
        let mut shown = Vec::new();
        for _ in 0..5 {
            let _img = step_demo(&mut d, 1.0, DEFAULT_W, DEFAULT_H);
            shown.push(d.idx);
        }

        // The last frame (index n-1) must appear in the shown sequence, and it
        // must be displayed BEFORE the wrap-back-to-0 that follows it.
        let last = n - 1;
        let pos = shown
            .iter()
            .position(|&i| i == last)
            .expect("the final frame index must be rendered at least once");
        assert_eq!(
            shown.get(pos + 1).copied(),
            Some(0),
            "after the last frame is shown, the very next step wraps to frame 0; shown={shown:?}"
        );
        // Concretely: 1.0s steps over t={0,1,2} render [1, 2, 0, 1, 2] — frame 2
        // (the last) is shown, then it loops to 0.
        assert_eq!(shown, vec![1, 2, 0, 1, 2], "deferred-wrap playback order");
    }

    #[test]
    fn step_demo_spawns_recorded_effects_into_the_particle_pool() {
        // A frame carrying an svc_particle burst + a TE_EXPLOSION temp entity
        // must fill the live particle pool when playback advances onto it, and
        // must NOT re-spawn while the same frame lingers, and must reset on wrap.
        use quake_rs::demo::{Demo, DemoFrame};
        use quake_rs::server::{te_consts, ParticleBurst, TempEntityEvent};

        let plain = |t: f32| DemoFrame {
            time: t,
            view_origin: [0.0, 0.0, 0.0],
            view_angles: [0.0, 0.0, 0.0],
            entities: Vec::new(),
            particles: Vec::new(),
            temp_entities: Vec::new(),
        };
        // Frame 1 (t=0.05) carries the effects; frames 0 and 2 are empty. Frame
        // times are one ~Quake tick apart so a 0.05s step advances exactly one
        // frame and the explosion's ramp ages by a realistic amount (not all the
        // way through its 8-frame life in a single huge step).
        let effect_frame = DemoFrame {
            time: 0.05,
            view_origin: [0.0, 0.0, 0.0],
            view_angles: [0.0, 0.0, 0.0],
            entities: Vec::new(),
            particles: vec![ParticleBurst {
                org: [0.0, 0.0, 0.0],
                dir: [0.0, 0.0, 0.0],
                color: 73,
                count: 20,
            }],
            temp_entities: vec![TempEntityEvent {
                te_type: te_consts::TE_EXPLOSION,
                pos: [10.0, 0.0, 0.0],
                color_start: 0,
                color_length: 0,
            }],
        };
        let demo = Demo {
            level_name: "test".into(),
            model_precache: vec![String::new(), "maps/test.bsp".into()],
            sound_precache: Vec::new(),
            frames: vec![plain(0.0), effect_frame, plain(0.10)],
        };
        let mut d = DemoPlay {
            bsp: render::demo_room(),
            palette: [[0u8; 3]; 256],
            demo,
            models: Vec::new(),
            colors: Vec::new(),
            elapsed: 0.0,
            idx: 0,
            particles: ParticleSystem::new(),
            prng: Lcg::new(1),
            last_spawned_idx: usize::MAX,
        };

        // Step 0.05s: lands on frame 1 (the effect frame). The burst (20) +
        // explosion (1024) particles populate the pool; after one tick of aging
        // the bulk of the 1024-particle explosion is still alive.
        let _ = step_demo(&mut d, 0.05, DEFAULT_W, DEFAULT_H);
        assert_eq!(d.idx, 1, "advanced onto the effect frame");
        let after_first = d.particles.len();
        assert!(
            after_first > 500,
            "the burst + 1024-particle explosion populate the pool (got {after_first})"
        );

        // A tiny step that holds us on frame 1 must NOT re-spawn the explosion
        // (the pool only shrinks as particles age — it never jumps back up).
        let _ = step_demo(&mut d, 0.001, DEFAULT_W, DEFAULT_H);
        assert_eq!(d.idx, 1, "still on the effect frame");
        assert!(
            d.particles.len() <= after_first,
            "no double-spawn: pool did not grow while the frame lingered"
        );

        // Drive 0.05s steps until playback wraps back to frame 0. After landing
        // on the last frame the very NEXT step wraps (deferred-wrap, as the
        // dedicated test above verifies); the wrap resets the pool, and frame 0
        // carries no effects, so the pool is empty afterwards.
        let mut wrapped = false;
        for _ in 0..6 {
            let _ = step_demo(&mut d, 0.05, DEFAULT_W, DEFAULT_H);
            if d.idx == 0 {
                wrapped = true;
                break;
            }
        }
        assert!(wrapped, "playback looped back to the first frame within a cycle");
        assert_eq!(d.idx, 0, "looped back to the first frame");
        assert!(
            d.particles.is_empty(),
            "wrap reset the particle pool (no stale explosion across the loop)"
        );
    }

    // -- dynamic render resolution (set_resolution + clamp + reallocation) ----

    #[test]
    fn clamp_resolution_clamps_into_envelope() {
        // In-range values pass through unchanged.
        assert_eq!(clamp_resolution(640, 400), (640, 400));
        assert_eq!(clamp_resolution(DEFAULT_W as i32, DEFAULT_H as i32), (320, 200));
        // Below the minimum clamps up; above the maximum clamps down.
        assert_eq!(clamp_resolution(0, 0), (MIN_W as usize, MIN_H as usize));
        assert_eq!(clamp_resolution(-100, -100), (320, 200));
        assert_eq!(clamp_resolution(99999, 99999).0, MAX_W as usize);
        // The pixel-budget cap: a max-width AND max-height request is trimmed so
        // w*h never exceeds MAX_PIXELS, never panicking.
        let (cw, ch) = clamp_resolution(MAX_W, MAX_H);
        assert!(
            (cw as i32).saturating_mul(ch as i32) <= MAX_PIXELS,
            "clamped {cw}x{ch} must respect the pixel cap"
        );
        assert!(cw >= MIN_W as usize && ch >= MIN_H as usize, "still a valid non-zero size");
        // i32::MAX in both dims must not overflow or panic.
        let (mw, mh) = clamp_resolution(i32::MAX, i32::MAX);
        assert!(mw <= MAX_W as usize && mh <= MAX_H as usize);
        assert!((mw as i32).saturating_mul(mh as i32) <= MAX_PIXELS);
    }

    #[test]
    fn set_resolution_reallocates_and_reports_new_size() {
        // Default boot size.
        assert_eq!(width(), DEFAULT_W as i32);
        assert_eq!(height(), DEFAULT_H as i32);

        // A valid in-range resolution is applied verbatim; width()/height() follow
        // and the framebuffer is exactly w*h*4 bytes.
        set_resolution(640, 400);
        assert_eq!(width(), 640);
        assert_eq!(height(), 400);
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().expect("app exists after set_resolution");
            assert_eq!(a.render_w, 640);
            assert_eq!(a.render_h, 400);
            assert_eq!(a.fb.len(), 640 * 400 * 4, "framebuffer reallocated to 640*400*4");
        });

        // Out-of-range input is clamped, not panicked: a huge request lands within
        // the envelope and the fb matches the clamped size.
        set_resolution(100000, 100000);
        let (w, h) = (width(), height());
        assert!((MIN_W..=MAX_W).contains(&w) && (MIN_H..=MAX_H).contains(&h));
        assert!(w.saturating_mul(h) <= MAX_PIXELS);
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            assert_eq!(a.fb.len(), (w as usize) * (h as usize) * 4);
        });

        // Back to the fast default.
        set_resolution(DEFAULT_W as i32, DEFAULT_H as i32);
        assert_eq!(width(), 320);
        assert_eq!(height(), 200);
    }

    #[test]
    fn boot_then_set_resolution_renders_larger_framebuffer() {
        // Boot the real walk (embedded pak). If the pak is unavailable in this
        // build the test would fail to boot; the workspace embeds a real PAK0.PAK.
        assert_eq!(boot(), 1, "boot the embedded e1m1 walk");
        // Boot keeps the fast default resolution.
        assert_eq!(width(), DEFAULT_W as i32);
        assert_eq!(height(), DEFAULT_H as i32);
        step(0.016);
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            assert_eq!(a.fb.len(), DEFAULT_W * DEFAULT_H * 4, "default fb is 320*200*4");
        });

        // Pick a larger resolution, then render: the framebuffer is now 640*400*4
        // and the scene rendered into all of it (the fb is fully written by step).
        set_resolution(640, 400);
        assert_eq!(width(), 640);
        assert_eq!(height(), 400);
        step(0.016);
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            assert_eq!(a.fb.len(), 640 * 400 * 4, "step renders into the 640*400 framebuffer");
            // Every alpha byte is 255 (step pushes opaque RGBA), proving the whole
            // larger buffer was painted, not just the old 320x200 region.
            assert!(a.fb.chunks_exact(4).all(|px| px[3] == 255), "full fb painted opaque");
        });
    }

    #[test]
    fn menu_left_right_cycle_resolution_and_apply() {
        // A fresh boot sits in the menu on Main. Navigate to Options and cycle the
        // Screen size row with menu_right; the engine's resolution must follow.
        assert_eq!(boot(), 1);
        assert_eq!(menu_visible(), 1, "boot enters the menu");
        // Default render size before touching anything.
        assert_eq!(width(), 320);
        assert_eq!(height(), 200);
        // Main cursor 0 is Single Player; move down to Options (item 2) and Enter.
        menu_down(); // -> 1 (Multiplayer)
        menu_down(); // -> 2 (Options)
        menu_select(); // enter Options
        assert_eq!(menu_visible(), 1);
        // menu_left/right off the resolution row shouldn't change the size; the
        // top Options row IS Screen size, so menu_right cycles to the next preset.
        menu_right();
        assert_eq!((width(), height()), (480, 300), "right cycles to the 480x300 preset");
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            assert_eq!(a.fb.len(), 480 * 300 * 4, "fb reallocated to the new preset");
        });
        // menu_left cycles back to 320x200.
        menu_left();
        assert_eq!((width(), height()), (320, 200), "left cycles back to the default");
    }

    // -- attract boot: menu over the playing demo (App-level menu) --------------

    /// Read `(mode, has_walk, has_demo, menu_visible)` from the live App.
    fn app_state() -> (u8, bool, bool, bool) {
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().expect("app exists");
            (a.mode, a.walk.is_some(), a.demo.is_some(), a.menu.visible)
        })
    }

    #[test]
    fn boot_attract_starts_demo_with_menu_open() {
        // boot_attract is how the page boots: the recorded demo plays with the
        // main menu OPEN over it (Quake's attract loop). The embedded pak ships
        // demo1.dem, so this builds the demo (returns 1) and lands in demo mode
        // with the menu visible.
        assert_eq!(boot_attract(), 1, "attract built the demo from the embedded pak");
        let (mode, has_walk, has_demo, vis) = app_state();
        assert_eq!(mode, 1, "attract boots into demo mode");
        assert!(has_demo, "the demo was built");
        assert!(!has_walk, "no walk is built for the attract demo");
        assert!(vis, "the menu is open over the playing demo");
        assert_eq!(menu_visible(), 1, "menu_visible reflects the App-level menu");

        // Stepping advances the demo (its frame index moves) WHILE the menu stays
        // open over it — the menu does not freeze the demo behind it.
        let idx_before = APP.with(|c| c.borrow().as_ref().unwrap().demo.as_ref().unwrap().idx);
        for _ in 0..40 {
            step(0.05);
        }
        let idx_after = APP.with(|c| c.borrow().as_ref().unwrap().demo.as_ref().unwrap().idx);
        assert_ne!(idx_before, idx_after, "the attract demo keeps playing behind the menu");
        assert_eq!(menu_visible(), 1, "the menu remains open over the demo");
    }

    #[test]
    fn boot_attract_new_game_switches_to_walk_with_menu_closed() {
        // From the attract loop, Single Player > New Game starts a fresh walk on
        // the start hub and closes the menu. Drive the same key path the page uses.
        assert_eq!(boot_attract(), 1);
        assert_eq!(app_state(), (1, false, true, true), "attract: demo mode, menu open");

        // Main screen cursor 0 = Single Player. Enter the SP submenu, then New Game
        // (its first item) is the default cursor 0 -> select.
        menu_select(); // Main > Single Player -> SinglePlayer screen
        assert_eq!(menu_visible(), 1, "still in the menu on the SinglePlayer screen");
        menu_select(); // SinglePlayer > New Game -> builds the walk, closes the menu

        let (mode, has_walk, _has_demo, vis) = app_state();
        assert_eq!(mode, 0, "New Game switches to walk mode");
        assert!(has_walk, "a fresh walk was built on the start hub");
        assert!(!vis, "the menu closed when the game started");
        assert_eq!(menu_visible(), 0, "menu_visible reflects the closed menu");

        // The walk renders a scene with the menu gone: step paints an opaque fb.
        step(0.016);
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            assert!(a.fb.chunks_exact(4).all(|px| px[3] == 255), "the walk scene renders");
        });

        // Esc reopens the menu over the running walk (menu_cancel from key_game).
        menu_cancel();
        assert_eq!(menu_visible(), 1, "Esc reopens the menu over the walk");
    }

    #[test]
    fn attract_menu_paints_pixels_over_the_demo_frame() {
        // The dispatcher overlays the menu on the demo frame. Prove the overlay
        // is non-empty (real menu pics from the embedded pak land on the frame):
        // render the SAME demo frame twice into identical Images, draw the menu
        // onto only one, and assert the two framebuffers differ.
        assert_eq!(boot_attract(), 1);
        // Step a little so the demo is on a populated frame (not the empty preroll).
        for _ in 0..10 {
            step(0.05);
        }
        let differ = APP.with(|c| {
            let mut b = c.borrow_mut();
            let a = b.as_mut().unwrap();
            assert!(a.menu.visible, "the attract menu is open");
            let (w, h) = (a.render_w, a.render_h);
            let d = a.demo.as_mut().unwrap();
            // Render the current demo frame with a tiny dt twice; with the menu
            // OFF and ON. (A tiny dt keeps both renders on the same frame.)
            let plain = step_demo(d, 0.0001, w, h);
            let mut withm = step_demo(d, 0.0001, w, h);
            let pal = a.active_palette().expect("demo palette");
            render::draw_menu(&mut withm, &a.menu, &a.menu_pics, a.conchars.as_ref(), a.clock, pal);
            // The two frames are the same scene; only the menu overlay differs.
            plain.rgb != withm.rgb
        });
        assert!(differ, "the menu overlay changes pixels on the demo frame");
    }

    #[test]
    fn boot_demo_keeps_menu_closed() {
        // The demo BUTTON (boot_demo) plays the demo with the menu CLOSED — the
        // clean-playback variant, distinct from the attract boot.
        assert_eq!(boot_demo(), 1, "the embedded demo builds");
        let (mode, _has_walk, has_demo, vis) = app_state();
        assert_eq!(mode, 1, "demo mode");
        assert!(has_demo);
        assert!(!vis, "boot_demo leaves the menu closed");
        assert_eq!(menu_visible(), 0);
    }

    // -----------------------------------------------------------------------
    // Drop-down console
    // -----------------------------------------------------------------------

    /// Read the player edict's `flags` field from the live walk (0.0 if no walk).
    fn player_flags() -> i32 {
        APP.with(|c| {
            c.borrow()
                .as_ref()
                .and_then(|a| a.walk.as_ref())
                .map(|w| w.server.vm.ent_get_float(w.player, "flags") as i32)
                .unwrap_or(0)
        })
    }

    /// Read a player-edict float field from the live walk (0.0 if no walk).
    fn player_field(name: &str) -> f32 {
        APP.with(|c| {
            c.borrow()
                .as_ref()
                .and_then(|a| a.walk.as_ref())
                .map(|w| w.server.vm.ent_get_float(w.player, name))
                .unwrap_or(0.0)
        })
    }

    fn console_scrollback() -> usize {
        APP.with(|c| {
            c.borrow().as_ref().map(|a| a.console.line_count()).unwrap_or(0)
        })
    }

    /// Type a whole line into the (open) console and submit it.
    fn run_console_line(line: &str) {
        for ch in line.chars() {
            console_char(ch as u32);
        }
        console_enter();
    }

    #[test]
    fn console_toggle_flips_visibility_and_gates_typing() {
        // ensure_app exists; start closed.
        ensure_app(|_| {});
        assert_eq!(console_visible(), 0, "console starts closed");
        // Typing while closed is ignored.
        console_char('x' as u32);
        APP.with(|c| assert_eq!(c.borrow().as_ref().unwrap().console.input(), ""));
        console_toggle();
        assert_eq!(console_visible(), 1, "toggle opens the console");
        // The backtick toggle char is never typed even while open.
        console_char('`' as u32);
        console_char('a' as u32);
        APP.with(|c| assert_eq!(c.borrow().as_ref().unwrap().console.input(), "a"));
        console_toggle();
        assert_eq!(console_visible(), 0, "toggle closes the console");
    }

    #[test]
    fn console_god_toggles_the_player_flags_bit() {
        assert_eq!(boot(), 1, "boot builds a walk from the embedded pak");
        console_toggle(); // open
        assert_eq!(console_visible(), 1);

        let before = player_flags();
        assert_eq!(before & FL_GODMODE, 0, "godmode starts off");
        run_console_line("god");
        let after = player_flags();
        assert_ne!(after & FL_GODMODE, 0, "the god command set FL_GODMODE on the player");
        // The command echoed the input line + its result into the scrollback.
        assert!(console_scrollback() >= 2, "god echoed the line and a result");

        // A second `god` toggles it back off.
        run_console_line("god");
        assert_eq!(player_flags() & FL_GODMODE, 0, "a second god clears FL_GODMODE");
    }

    #[test]
    fn console_unknown_command_prints_an_error() {
        assert_eq!(boot(), 1);
        console_toggle();
        APP.with(|c| c.borrow_mut().as_mut().unwrap().console.clear());
        run_console_line("frobnicate now");
        // Exactly two lines: the echoed "]frobnicate now" and the error message.
        // (A known command would echo the line + its own variable output; an
        // unknown one always produces precisely the echo + one error line.)
        let lines = console_scrollback();
        assert_eq!(lines, 2, "unknown command echoes the line and one error line");
    }

    #[test]
    fn console_give_changes_the_field() {
        assert_eq!(boot(), 1);
        console_toggle();
        // give h 100 sets the player's health field.
        run_console_line("give h 100");
        assert_eq!(player_field("health"), 100.0, "give h set health");
        // give s 50 sets ammo_shells.
        run_console_line("give s 50");
        assert_eq!(player_field("ammo_shells"), 50.0, "give s set ammo_shells");
        // give 2 grants + selects the shotgun (items bit 1, weapon bit 1).
        run_console_line("give 2");
        let items = player_field("items") as i32;
        assert_ne!(items & IT_SHOTGUN, 0, "give 2 set the shotgun items bit");
        assert_eq!(player_field("weapon") as i32, IT_SHOTGUN, "give 2 selected the shotgun");
    }

    #[test]
    fn console_impulse_queues_next_impulse() {
        assert_eq!(boot(), 1);
        console_toggle();
        run_console_line("impulse 9");
        let n = APP.with(|c| {
            c.borrow().as_ref().unwrap().walk.as_ref().unwrap().next_impulse
        });
        assert_eq!(n, 9, "impulse 9 queued the give-all cheat impulse");
    }

    #[test]
    fn console_clear_and_echo_and_noclip_fly_kill() {
        assert_eq!(boot(), 1);
        console_toggle();
        run_console_line("echo hello world");
        assert!(console_scrollback() >= 2, "echo printed text");
        run_console_line("clear");
        // After clear, only the echoed "]clear" line (pushed before exec) remains
        // — clear empties everything that came before it.
        assert_eq!(console_scrollback(), 0, "clear empties the scrollback");

        // noclip toggles movetype WALK <-> NOCLIP.
        run_console_line("noclip");
        assert_eq!(player_field("movetype"), MOVETYPE_NOCLIP, "noclip set NOCLIP");
        run_console_line("noclip");
        assert_eq!(player_field("movetype"), MOVETYPE_WALK, "noclip toggled back to WALK");
        // fly toggles WALK <-> FLY.
        run_console_line("fly");
        assert_eq!(player_field("movetype"), MOVETYPE_FLY, "fly set FLY");
        // kill zeroes health.
        run_console_line("kill");
        assert_eq!(player_field("health"), 0.0, "kill zeroed health");
    }

    #[test]
    fn console_map_failure_keeps_level_and_prints_not_found() {
        assert_eq!(boot(), 1);
        console_toggle();
        let had_walk = APP.with(|c| c.borrow().as_ref().unwrap().walk.is_some());
        assert!(had_walk);
        run_console_line("map nosuchmap");
        // The walk is unchanged and the console stays OPEN (failure path).
        let still = APP.with(|c| c.borrow().as_ref().unwrap().walk.is_some());
        assert!(still, "a missing map leaves the current walk in place");
        assert_eq!(console_visible(), 1, "a failed map keeps the console open");
    }

    #[test]
    fn console_command_guards_missing_walk() {
        // A fresh app with NO walk: a game command prints "no active game", no panic.
        ensure_app(|a| {
            a.walk = None;
            a.console.open = true;
            a.console.clear();
        });
        run_console_line("god");
        // Echoed line + "no active game".
        assert!(console_scrollback() >= 2, "god with no walk prints a guard message");
    }

    #[test]
    fn boot_loads_the_conback_from_the_pak() {
        // The console background (gfx/conback.lmp) loads once alongside the menu
        // assets on first boot. The embedded pak ships it, so after boot the App
        // holds a parsed 320x200 conback — what draw_console paints across the top.
        assert_eq!(boot(), 1);
        APP.with(|c| {
            let b = c.borrow();
            let cb = b.as_ref().unwrap().conback.as_ref();
            assert!(cb.is_some(), "gfx/conback.lmp loaded from the embedded pak");
            let cb = cb.unwrap();
            assert!(cb.width > 0 && cb.height > 0, "conback has real dimensions");
        });
    }

    #[test]
    fn step_draws_console_over_everything_when_open() {
        assert_eq!(boot(), 1);
        // Render one frame with the console CLOSED, then OPEN; the open frame must
        // differ (the panel paints over the top of the scene + menu).
        let (w, h) = APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            (a.render_w, a.render_h)
        });
        step(0.016);
        let closed: Vec<u8> = APP.with(|c| c.borrow().as_ref().unwrap().fb.clone());
        console_toggle();
        APP.with(|c| c.borrow_mut().as_mut().unwrap().console.println("test line"));
        step(0.016);
        let open: Vec<u8> = APP.with(|c| c.borrow().as_ref().unwrap().fb.clone());
        assert_eq!(closed.len(), w * h * 4);
        assert_ne!(closed, open, "the open console changes the rendered frame");
        // Pixels in the very top row (the panel) are present (non-uniform / drawn).
        let top_changed = closed[..w * 4] != open[..w * 4];
        assert!(top_changed, "the console panel paints the top region of the frame");
    }
}
