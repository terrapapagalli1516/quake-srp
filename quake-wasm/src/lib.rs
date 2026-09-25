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
use quake_rs::render::{self, build_gamma_table, Console, Menu, MenuPics};
use quake_rs::server::Server;
use quake_rs::tent::{BeamSegment, Beams};
use quake_rs::wad::Qpic;

mod bench;
use bench::Phase;
mod cl_demo;
mod cl_tent;
mod cl_walk;
mod console;
mod host_cmd;
mod input;
mod menu;
mod savegame;
mod snd_dma;
mod vid;
#[cfg(test)]
mod test_util;

use cl_demo::step_demo;
use cl_walk::step_walk;
use input::{derive_key_move, KeyMove};
use snd_dma::{bump_sound_generation, queue_static_sounds, SND_QUEUE, STOP_SND_QUEUE};
use vid::{DEFAULT_H, DEFAULT_W};

static PAK: &[u8] = include_bytes!("../../quake-data/ID1/PAK0.PAK");

const WALK_MAP: &str = "maps/e1m1.bsp";
const DEMO_FILE: &str = "demo1.dem";

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
    /// The `conchars` font (extracted once from `gfx_wad`) for the on-screen
    /// message overlay — centerprint (centered) + notify lines (top-left).
    conchars: Option<Qpic>,
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
    /// Parsed sprite models (`.spr`) keyed by name, cached like `model_cache` so a
    /// sprite (the explosion flash, bubbles) parses once and backs every instance.
    sprite_cache: HashMap<String, Option<quake_rs::spr::Sprite>>,
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
    /// The spawn parms captured when this level was ENTERED (the alive player's
    /// inventory at level start). A single-player respawn (`localcmd("restart")`)
    /// reloads the current level with THESE, since a dead player's state is empty.
    entry_parms: [f32; quake_rs::server::NUM_SPAWN_PARMS],
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
    /// This frame's bindings-derived keyboard input (CL_BaseMove/CL_AdjustAngles
    /// over the page-held keys), refreshed by `step` before `step_walk` runs.
    key_move: KeyMove,
    /// The `viewsize` cvar this frame (the Options "Screen size" slider),
    /// refreshed by `step` from the menu before stepping, like `key_move`:
    /// [`render::calc_refdef`] turns it into the 3-D view rectangle and how
    /// much status bar shows.
    viewsize: f32,
    /// Accumulated mouse-strafe sidemove units (in_win.c IN_MouseMove's
    /// `cmd->sidemove += m_side.value * mouse_x` when lookstrafe / +strafe route
    /// mouse X away from yaw). Drained into the next UserCmd then cleared.
    mouse_side: f32,
    /// Accumulated mouse forwardmove units (IN_MouseMove's else branch:
    /// `cmd->forwardmove -= m_forward.value * mouse_y` while +strafe holds mouse
    /// Y out of the pitch path). Drained like `mouse_side`.
    mouse_fwd: f32,
    /// Pitch drift active (view.c `!cl.nodrift`): centerview or a lookspring
    /// pointer-unlock started it; the view re-levels at `pitch_vel` deg/sec until
    /// it reaches 0 or mouse/keyboard look stops it (V_StopPitchDrift).
    pitch_drift: bool,
    /// `cl.pitchvel` — the drift rate, seeded with [`V_CENTERSPEED`] and
    /// accelerated by it each second while drifting (V_DriftPitch).
    pitch_vel: f32,
    /// Full-screen damage-flash intensity (Quake's `CSHIFT_DAMAGE` percent,
    /// 0..150): bumped when the player loses health/armour and faded each frame.
    damage_blend: f32,
    /// The damage-flash tint colour (`V_ParseDamage` picks (200,100,100) when armour
    /// absorbs most, (220,50,50) for armour-only, (255,0,0) for pure blood).
    damage_color: [u8; 3],
    /// Player health last frame (NaN until known / after a level change), used with
    /// `last_armor` to split this frame's damage into blood vs armour for the flash.
    last_health: f32,
    /// Player armour last frame (NaN until known / after a level change).
    last_armor: f32,
    /// Stair-step view smoothing accumulator (`view.c` V_CalcRefdef `oldz`): the eye
    /// Z lags the player Z by up to 12 units while climbing so stairs glide instead
    /// of jolting. NaN until the first frame establishes it.
    oldz: f32,
    /// Current centered message (`centerprint`) + the clock time it expires at
    /// (Quake's `scr_centertime` ~2s); replaced by the next centerprint. Drawn
    /// centered over the view.
    centerprint: Option<(String, f32)>,
    /// The fading top-left notify lines (`bprint`/`sprint`): text + expiry clock
    /// (Quake's `con_notifytime` ~3s), capped to the last few.
    notify: Vec<(String, f32)>,
    /// The in-progress notify line (Con_Print model): bprint/sprint text accumulates
    /// here and only breaks into a notify line on '\n'. Quake item pickups print via
    /// several `sprint` calls ("You receive ", "25", " health\n"); the C console
    /// joins them into ONE line, so we must not emit one notify line per call.
    notify_pending: String,
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
    /// The beam temp-entity slots (`cl_beams`): lightning bolts the drained
    /// `TE_LIGHTNING1/2/3` / `TE_BEAM` events refresh ([`Beams::parse_beam`])
    /// and [`step_walk`] expands into bolt-model instances each frame
    /// (`CL_UpdateTEnts`). Cleared on changelevel/restart (`CL_ClearState`).
    beams: Beams,
    /// Reused per-frame scratch for the expanded beam pieces (no per-frame
    /// allocation on the common no-beam frames; `Beams::update` clears it).
    beam_scratch: Vec<BeamSegment>,
    /// `cl.intermission` (client.h): 0 = playing, 1 = the level-complete stats
    /// overlay (svc_intermission), 2 = the episode finale text + plaque
    /// (svc_finale), 3 = cutscene text only (svc_cutscene). While non-zero the
    /// view is the QC-placed intermission camera (V_CalcIntermissionRefdef): no
    /// bob/roll/punch, no viewmodel, no status bar.
    intermission: u8,
    /// `cl.completed_time` — the clock latched when the intermission started
    /// (the overlay's minutes:seconds completion time).
    completed_time: f32,
    /// The `svc_finale`/`svc_cutscene` text (`SCR_CenterPrint`'d in the C),
    /// revealed at `scr_printspeed` (8) chars/sec from `finale_start`.
    finale_text: String,
    /// `scr_centertime_start` — the clock when the finale text began revealing.
    finale_start: f32,
    /// `svc_sellscreen` arrived this frame: the C ran `Cmd_ExecuteString("help")`,
    /// i.e. popped the Help/Ordering menu — the `step` dispatcher (which owns the
    /// menu) takes this flag and opens it.
    pending_sellscreen: bool,
    /// `gfx/complete.lmp` — the "Level Complete" banner (Sbar_IntermissionOverlay).
    pic_complete: Option<Qpic>,
    /// `gfx/inter.lmp` — the Time/Secrets/Kills intermission plaque.
    pic_inter: Option<Qpic>,
    /// `gfx/finale.lmp` — the finale plaque (Sbar_FinaleOverlay).
    pic_finale: Option<Qpic>,
}

/// Recorded-demo playback state.
struct DemoPlay {
    bsp: Bsp,
    palette: [[u8; 3]; 256],
    demo: Demo,
    /// The archive, kept open so the recorded `svc_sound` one-shots can load
    /// their WAV bytes on demand — the demo's audio runs through the SAME
    /// `queue_sounds` path live play uses.
    pak: Pak,
    /// Parsed model per precache index (None for non-`.mdl` / missing).
    models: Vec<Option<Mdl>>,
    /// Parsed sprite per precache index (None for non-`.spr` / missing); the boot
    /// demo's explosion flashes (s_explod.spr) render from these.
    sprites: Vec<Option<quake_rs::spr::Sprite>>,
    /// `gfx/colormap.lmp` — the 64-row shade LUT. The demo path must thread it like
    /// the live walk does, else world surfaces render overbright (linear fallback)
    /// instead of through id's no-overbright colormap shading.
    colormap: Option<Vec<u8>>,
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
    /// The beam temp-entity slots (`cl_beams`) replayed from the recorded
    /// `TE_LIGHTNING1/2/3` / `TE_BEAM` stream; expanded into bolt-model
    /// instances each frame like the live walk. Cleared on the demo loop wrap.
    beams: Beams,
    /// Reused per-frame scratch for the expanded beam pieces.
    beam_scratch: Vec<BeamSegment>,
    /// `gfx.wad` (the big digit pics) for a recorded intermission's stats
    /// overlay; `None` degrades to no overlay, never a panic.
    gfx_wad: Option<quake_rs::wad::Wad2>,
    /// The conchars font for a recorded finale's revealed center string.
    conchars: Option<Qpic>,
    /// `gfx/complete.lmp` / `gfx/inter.lmp` / `gfx/finale.lmp` — the plaques a
    /// recorded intermission/finale frame draws (Sbar_Intermission/FinaleOverlay).
    pic_complete: Option<Qpic>,
    pic_inter: Option<Qpic>,
    pic_finale: Option<Qpic>,
    /// `cl.cshifts[CSHIFT_DAMAGE].percent` for the recorded POV: bumped by each
    /// recorded `svc_damage` (V_ParseDamage: `+= 3*count`, clamped 0..150) and
    /// faded `dt*150` per rendered frame (V_UpdatePalette), exactly like the
    /// live walk's damage flash.
    damage_blend: f32,
    /// The damage tint V_ParseDamage picked (armour-dominant pink / armour
    /// orange-red / pure-blood red).
    damage_color: [u8; 3],
    /// `v_dmg_time` / `v_dmg_roll` / `v_dmg_pitch` (view.c): the directional
    /// view kick a recorded svc_damage applies, decaying over `v_kicktime`.
    v_dmg_time: f32,
    v_dmg_roll: f32,
    v_dmg_pitch: f32,
    /// Stair-step smoothing accumulator (V_CalcRefdef `oldz`) for the recorded
    /// view entity; NaN until the first frame establishes it.
    oldz: f32,
    /// Current centerprint + expiry (recorded `svc_centerprint`, scr_centertime
    /// ~2 s on the demo's recorded frame clock).
    centerprint: Option<(String, f32)>,
    /// Notify lines + expiries (recorded `svc_print`, con_notifytime ~3 s).
    notify: Vec<(String, f32)>,
    /// The in-progress notify line (Con_Print model: break only on '\n') —
    /// recorded pickups print as several svc_print fragments.
    notify_pending: String,
    /// The `viewsize` cvar this frame (the Options "Screen size" slider),
    /// refreshed by `step` from the menu before stepping, like `key_move`:
    /// [`render::calc_refdef`] turns it into the 3-D view rectangle and how
    /// much status bar shows.
    viewsize: f32,
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
    /// `host_time` (host.c): the accumulated CLAMPED frame time (seconds) —
    /// `step`'s `dt` after Host_FilterTime's 0.1 s cap — advanced every `step`
    /// regardless of mode. Drives the menudot spinner (`(int)(host_time*10) % 6`
    /// in `M_Main_Draw` and friends), which keeps turning over a frozen frame.
    clock: f32,
    /// `realtime` (host.c): the UNCLAMPED wall clock (seconds) — `step`'s raw
    /// `dt` summed, before Host_FilterTime caps the frame time. Drives every
    /// flashing cursor the C times on `realtime`: the menu cursors
    /// (`12 + ((int)(realtime*4)&1)`) and the console input cursor
    /// (`Con_DrawInput`, `con_cursorspeed` 4).
    realtime: f64,
    /// `oldrealtime` (host.c): `realtime` when the last host frame ran —
    /// [`host_filter_time`]'s gate measures the time since then.
    oldrealtime: f64,
    /// Current render resolution (runtime; defaults to [`DEFAULT_W`] x
    /// [`DEFAULT_H`]). The scene renders at this size and the framebuffer is
    /// `render_w * render_h * 4` RGBA bytes, reallocated whenever it changes.
    render_w: usize,
    render_h: usize,
    fb: Vec<u8>, // RGBA, render_w*render_h*4
    /// The page-held key states by Quake keynum (keys.c `keydown[256]`), fed by
    /// [`key_down`]/[`key_up`]. Mode-independent (held keys survive a level
    /// change) and consulted through the menu's binding table each `step`.
    keys_held: [bool; 256],
    /// The gamma the current [`App::gamma_table`] was built for (V_CheckGamma's
    /// `oldgammavalue`): the table rebuilds only when the menu's `v_gamma`
    /// actually changes.
    gamma_value: f32,
    /// The 256-entry gamma LUT (view.c `gammatable`), applied where the finished
    /// frame is packed into the presented RGBA framebuffer — the port's
    /// hardware-palette boundary (`VID_ShiftPalette`). Identity at gamma 1.0,
    /// where the pack skips it entirely (byte-exact default).
    gamma_table: [u8; 256],
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

/// The embedded pak as a borrowed handle: `from_static` slices the
/// `include_bytes!` image in place, so a boot, a map load, a sound load and
/// every `Pak` clone (Walk, DemoPlay, `Server::with_pak`) cost a directory
/// parse, never an 18.7 MB copy.
fn pak() -> Option<quake_rs::pak::Pak> {
    quake_rs::pak::Pak::from_static("pak0.pak".into(), PAK).ok()
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
    // The 6 Help/Ordering pages (gfx/help0.lmp..help5.lmp). Each optional.
    let mut help: [Option<Qpic>; render::NUM_HELP_PAGES] = Default::default();
    for (i, slot) in help.iter_mut().enumerate() {
        *slot = lmp(&format!("gfx/help{i}.lmp"));
    }
    let pics = MenuPics {
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

/// Shared tail of the walk builders ([`build_walk_map`] / the savegame load
/// path): load the render-side assets (palette, gfx.wad, colormap, conchars,
/// plaque pics) from the pak and assemble a fresh [`Walk`] around an
/// already-built server. Every per-session field starts from its clean-slate
/// default (no particles/dlights/beams/notify, clock 0). Returns `None` only
/// when the palette is missing/unparseable (nothing could render).
#[allow(clippy::too_many_arguments)]
fn assemble_walk(
    pak: Pak,
    map: String,
    server: Server,
    player: i32,
    entry_parms: [f32; quake_rs::server::NUM_SPAWN_PARMS],
    bsp: Bsp,
    yaw: f32,
    pitch: f32,
) -> Option<Walk> {
    let read = |n: &str| pak.read_file(n).ok().flatten();
    let palette = render::parse_palette(&read("gfx/palette.lmp")?)?;
    // The HUD pics live in gfx.wad; parse it once (None if absent/unparseable).
    let gfx_wad = read("gfx.wad").and_then(|b| quake_rs::wad::Wad2::parse(b).ok());
    let colormap = read("gfx/colormap.lmp");
    let conchars = gfx_wad.as_ref().and_then(render::conchars_pic);
    // The intermission/finale plaques are pak `.lmp` pics (Draw_CachePic in the
    // C), loaded once like the menu pics; any absent one just doesn't draw.
    let lmp = |n: &str| -> Option<Qpic> { read(n).and_then(|b| Qpic::parse(&b).ok()) };
    let pic_complete = lmp("gfx/complete.lmp");
    let pic_inter = lmp("gfx/inter.lmp");
    let pic_finale = lmp("gfx/finale.lmp");
    Some(Walk {
        server,
        bsp,
        palette,
        gfx_wad,
        colormap,
        conchars,
        pak,
        model_cache: HashMap::new(),
        bmodel_cache: HashMap::new(),
        sprite_cache: HashMap::new(),
        trail_org: HashMap::new(),
        tracercount: 0,
        map_name: map,
        entry_parms,
        player,
        yaw,
        pitch,
        in_fwd: 0.0,
        in_side: 0.0,
        in_attack: false,
        in_jump: false,
        in_down: false,
        next_impulse: 0,
        // Bindings-driven input state (CL_BaseMove derivation; ship/options-menu).
        key_move: KeyMove::default(),
        mouse_side: 0.0,
        mouse_fwd: 0.0,
        pitch_drift: false,
        pitch_vel: 0.0,
        damage_blend: 0.0,
        damage_color: [255, 0, 0],
        last_health: f32::NAN,
        last_armor: f32::NAN,
        oldz: f32::NAN,
        centerprint: None,
        notify: Vec::new(),
        notify_pending: String::new(),
        viewsize: render::VIEWSIZE_DEFAULT,
        clock: 0.0,
        particles: ParticleSystem::new(),
        prng: Lcg::new(0x9E37_79B9),
        dlights: DynamicLights::new(),
        beams: Beams::new(),
        beam_scratch: Vec::new(),
        intermission: 0,
        completed_time: 0.0,
        finale_text: String::new(),
        finale_start: 0.0,
        pending_sellscreen: false,
        pic_complete,
        pic_inter,
        pic_finale,
    })
}

/// Build a live walk on `map` (a `maps/*.bsp` pak path): boot uses [`WALK_MAP`];
/// New Game uses [`render::NEW_GAME_MAP`] (the `start` hub).
fn build_walk_map(map: &str) -> Option<Walk> {
    let pak = pak()?;
    let read = |n: &str| pak.read_file(n).ok().flatten();
    let bsp = Bsp::parse(&read(map)?).ok()?;
    let bsp_sim = Bsp::parse(&read(map)?).ok()?;
    let progs = Progs::parse(&read("progs.dat")?).ok()?;
    let (_spawn, yaw) = player_start(&bsp.entities).unwrap_or(([0.0, 0.0, 0.0], 0.0));

    // A live server: spawn the map's entities, then connect the local player.
    // Pass the pak so external brush-model item boxes (b_*.bsp) collide + take
    // damage (the explosive box becomes shootable).
    let mut server = Server::with_pak(bsp_sim, progs, Some(pak.clone())).ok()?;
    // Discard any static-sound registrations a previously FAILED spawn left in
    // the thread-local registry, so this level's drain below is exactly its own.
    let _ = server.drain_static_sounds();
    // SV_SpawnServer set world.model + the mapname global before loading the
    // entities (the QuakeC episode-end finale check reads world.model).
    server.set_map_name(map);
    server.spawn_entities().ok()?;
    let player = server.connect_client().ok()?;
    // Capture the level-entry spawn parms (the just-connected, full-state player) so
    // a single-player respawn can reload THIS level with them.
    let entry_parms = server.save_spawn_parms();
    // The signon-sequence physics frames the C runs between PutClientInServer and
    // the first rendered frame (Host_Spawn_f/Host_Begin_f each precede an
    // SV_Physics tick before signon 4 re-enables drawing). Without them the
    // just-spawned player — placed at spot.origin + '0 0 1', ~5 units up on the
    // start map — falls to the floor ON SCREEN over the first frames: the
    // reported one-time texture/lighting "pop" (every surface resamples as the
    // eye drops). Frame 0 must render the settled WinQuake pose.
    server.run_signon_frames();

    // The level is committed past this point (nothing below fails). Tear down
    // the previous level/mode's looping audio and register this level's placed
    // `ambientsound()` loops (torches, wind, hums) for the page to start —
    // PF_ambientsound wrote these into the signon ONCE; the QuakeC registered
    // them all during spawn_entities, so one drain captures them all.
    bump_sound_generation();
    let statics = server.drain_static_sounds();
    queue_static_sounds(&pak, &statics);
    // Drop one-shot events the spawn + settle ticks queued, like the
    // changelevel/restart paths do (in the C the client misses signon-era
    // datagram sounds while not yet `spawned`; tick 2's are technically
    // deliverable there — dropping both is a deliberate, inaudible-on-id-maps
    // simplification, kept identical across all three walk-building paths).
    let _ = server.drain_sounds();
    let _ = server.drain_particles();
    let _ = server.drain_temp_entities();
    let _ = server.drain_messages();
    let _ = server.drain_svc_events();

    assemble_walk(pak, map.to_string(), server, player, entry_parms, bsp, yaw, 0.0)
}

fn build_demo() -> Option<DemoPlay> {
    let pak = pak()?;
    let read = |n: &str| pak.read_file(n).ok().flatten();
    let demo_bytes = read(DEMO_FILE)?;
    let demo = parse_demo(&demo_bytes).ok()?;
    let map = demo.map_name()?.to_string();
    let bsp = Bsp::parse(&read(&map)?).ok()?;
    let palette = render::parse_palette(&read("gfx/palette.lmp")?)?;

    // Load a model (alias or sprite) + colour per precache index.
    let mut models = Vec::with_capacity(demo.model_precache.len());
    let mut sprites: Vec<Option<quake_rs::spr::Sprite>> =
        Vec::with_capacity(demo.model_precache.len());
    let mut colors = Vec::with_capacity(demo.model_precache.len());
    for name in &demo.model_precache {
        if name.ends_with(".mdl") {
            models.push(read(name).and_then(|b| Mdl::parse(&b).ok()));
            sprites.push(None);
        } else if name.ends_with(".spr") {
            models.push(None);
            sprites.push(read(name).and_then(|b| quake_rs::spr::Sprite::parse(&b).ok()));
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
                .filter(|md| md.header.flags & quake_rs::demo::EF_ROTATE != 0)
                .map(|_| i)
        })
        .collect();
    let demo = quake_rs::demo::parse_demo_interpolated(&demo_bytes, 60.0, &rotating).ok()?;
    if demo.frames.is_empty() {
        return None;
    }
    // Demo committed (nothing below fails): tear down the previous level/mode's
    // looping audio and register the demo signon's `svc_spawnstaticsound` loops
    // (CL_ParseStaticSound ran these on the live client during demo playback
    // too — the e1m3 demo has its own torches).
    bump_sound_generation();
    queue_static_sounds(&pak, &demo.static_sounds);
    // The overlay assets for a recorded intermission/finale (each optional —
    // a demo without one never touches them).
    let gfx_wad = read("gfx.wad").and_then(|b| quake_rs::wad::Wad2::parse(b).ok());
    let conchars = gfx_wad.as_ref().and_then(render::conchars_pic);
    let lmp = |n: &str| -> Option<Qpic> { read(n).and_then(|b| Qpic::parse(&b).ok()) };
    // Resolve everything that reads the pak BEFORE the struct literal so the
    // `read`/`lmp` closure borrows end and `pak` can move into the DemoPlay.
    let colormap = read("gfx/colormap.lmp");
    let pic_complete = lmp("gfx/complete.lmp");
    let pic_inter = lmp("gfx/inter.lmp");
    let pic_finale = lmp("gfx/finale.lmp");
    Some(DemoPlay {
        bsp,
        palette,
        demo,
        pak,
        models,
        sprites,
        colormap,
        colors,
        elapsed: 0.0,
        idx: 0,
        particles: ParticleSystem::new(),
        prng: Lcg::new(0x9E37_79B9),
        last_spawned_idx: usize::MAX,
        beams: Beams::new(),
        beam_scratch: Vec::new(),
        pic_complete,
        pic_inter,
        pic_finale,
        gfx_wad,
        conchars,
        damage_blend: 0.0,
        damage_color: [255, 0, 0],
        v_dmg_time: 0.0,
        v_dmg_roll: 0.0,
        v_dmg_pitch: 0.0,
        oldz: f32::NAN,
        centerprint: None,
        notify: Vec::new(),
        notify_pending: String::new(),
        viewsize: render::VIEWSIZE_DEFAULT,
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
                realtime: 0.0,
                oldrealtime: 0.0,
                render_w: DEFAULT_W,
                render_h: DEFAULT_H,
                fb: vec![0u8; DEFAULT_W * DEFAULT_H * 4],
                keys_held: [false; 256],
                gamma_value: 1.0,
                gamma_table: build_gamma_table(1.0),
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
    // samples can't play after the switch (pending stop requests included).
    SND_QUEUE.with(|q| q.borrow_mut().clear());
    STOP_SND_QUEUE.with(|q| q.borrow_mut().clear());
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
            // Quake boots INTO the menu over the e1m1 frame. Reset the menu's
            // NAVIGATION (closed, main screen, cursor 0) and open it over the
            // walk — but KEEP the player's options, key rebinds, and slot
            // comments: in the C a map start never touches cvars/keybindings
            // (they're host state), so re-booting must not wipe them.
            a.menu.reset_nav();
            a.menu.open();
            // PRESERVE the player's chosen resolution across the re-boot: keep the
            // current framebuffer size (the source of truth) and point the fresh
            // menu's current video mode at it, instead of snapping back to DEFAULT.
            // (Re-booting used to revert a menu-picked resolution; it no longer does.)
            a.menu.sync_resolution(a.render_w as i32, a.render_h as i32);
        }
    });
    ok as i32
}

/// Start recorded-demo playback (demo1.dem / e1m3). Returns 1 on success.
#[no_mangle]
pub extern "C" fn boot_demo() -> i32 {
    // Clean slate: drop any sounds still queued from a previous mode
    // (pending stop requests included).
    SND_QUEUE.with(|q| q.borrow_mut().clear());
    STOP_SND_QUEUE.with(|q| q.borrow_mut().clear());
    let d = build_demo();
    let ok = d.is_some();
    ensure_app(|a| {
        a.ensure_menu_assets();
        a.demo = d;
        if a.demo.is_some() {
            a.mode = 1;
            // The demo button plays the demo with the menu CLOSED (clean
            // playback). `boot_attract` is the variant that opens the menu over it.
            // Navigation-only reset: options/bindings/slot comments survive (the
            // C never resets cvars or keybindings on a mode change). PRESERVE the
            // chosen resolution too (keep the live framebuffer) and point the
            // menu's current video mode at it so it's correct when the player
            // next opens Video Options.
            a.menu.reset_nav();
            a.menu.sync_resolution(a.render_w as i32, a.render_h as i32);
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
    // Clean slate: drop any sounds still queued from a previous mode
    // (pending stop requests included).
    SND_QUEUE.with(|q| q.borrow_mut().clear());
    STOP_SND_QUEUE.with(|q| q.borrow_mut().clear());
    let d = build_demo();
    let built = d.is_some();
    ensure_app(|a| {
        a.ensure_menu_assets();
        if let Some(d) = d {
            a.demo = Some(d);
            a.mode = 1;
            // The menu overlays the PLAYING attract demo. Navigation-only reset
            // (options/bindings survive a re-entry to the attract loop). PRESERVE
            // the chosen resolution (keep the live framebuffer) and sync the
            // menu's current video mode to it. On the very first load the
            // framebuffer is at DEFAULT; the page then restores any saved
            // resolution over it.
            a.menu.reset_nav();
            a.menu.open();
            a.menu.sync_resolution(a.render_w as i32, a.render_h as i32);
        }
    });
    if built {
        1
    } else {
        // No demo (missing/bad pak): still give the player a menu over a frame.
        boot()
    }
}

/// `1` while the live, player-controlled WALK is the active mode; `0` during
/// demo playback / the attract loop (and before any boot). The page gates its
/// pointer-lock requests and the "click to capture mouse" chip on this — the
/// mouse only drives the camera in walk mode, so that is the only mode where a
/// canvas click should capture it. Covers every walk-building path (boot /
/// New Game / `map` console command), since each sets `mode = 0` with the walk.
#[no_mangle]
pub extern "C" fn in_walk_mode() -> i32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| (a.mode == 0 && a.walk.is_some()) as i32)
            .unwrap_or(0)
    })
}

/// `Host_FilterTime` (host.c): the most a single frame may advance the game —
/// a longer real frame (a hitch, a backgrounded tab) is clamped to 0.1 s of
/// `host_frametime` while `realtime` still takes the whole elapsed time.
const HOST_FRAMETIME_MAX: f32 = 0.1;
/// `Host_FilterTime`'s lower clamp on `host_frametime` ("don't allow really
/// long or short frames").
const HOST_FRAMETIME_MIN: f32 = 0.001;
/// `Host_FilterTime`'s frame cap: no host frame runs until 1/72 s of real
/// time has passed since the last one ("framerate is too high").
const HOST_FRAME_INTERVAL: f64 = 1.0 / 72.0;
/// DEVIATION (the only one in the gate): a frame may run up to 1 ms before
/// the full 1/72 s. The C polls a free-running clock; this port only gets a
/// chance to run on a display refresh, so the gate sees whole vsync
/// intervals. On a 144 Hz display two of them come to 13.889 ms, a hair
/// either side of 1/72 s, and without slack the rate judders between 72 and
/// 48 fps. The slack has a window: above 0.56 ms a 75 Hz display (13.33 ms)
/// runs every refresh instead of every other; below 1.39 ms 240 Hz (3 vsyncs
/// = 12.5 ms) and 165 Hz (2 = 12.12 ms) stay gated, at 60 and 55 fps, instead
/// of 80 and 82.5. 1 ms sits mid-window, ~0.4 ms from either edge for
/// timestamp jitter. Rates per display are pinned by
/// `host_filter_time_caps_every_refresh_rate_at_a_steady_cadence`.
const HOST_FRAME_TOLERANCE: f64 = 0.001;

/// `Host_FilterTime` (host.c): given `realtime` (already advanced by this
/// call's raw time), decide whether a host frame runs. `None` = "framerate is
/// too high": do nothing this call. `Some(host_frametime)` = run a frame that
/// advances the game by the real time since the last frame, clamped to
/// [0.001, 0.1]; `oldrealtime` moves up to `realtime`, dropping any
/// overshoot, as the C does.
fn host_filter_time(realtime: f64, oldrealtime: &mut f64) -> Option<f32> {
    let elapsed = realtime - *oldrealtime;
    if elapsed < HOST_FRAME_INTERVAL - HOST_FRAME_TOLERANCE {
        return None;
    }
    *oldrealtime = realtime;
    Some((elapsed as f32).clamp(HOST_FRAMETIME_MIN, HOST_FRAMETIME_MAX))
}

/// One call per display refresh: `dt` is the raw wall-clock time since the
/// previous call. Like `Host_Frame`, it all goes to `realtime`, then
/// [`host_filter_time`] decides whether a frame runs: at most 72 per second
/// (see [`HOST_FRAME_TOLERANCE`]), each advancing the game (world, demo,
/// `host_time`) by the time since the last one, clamped to [0.001, 0.1].
/// Returns 1 when a frame ran and the framebuffer holds it, 0 when the cap
/// skipped this call (the page then has nothing new to present).
///
/// `dt = 0` (or a non-finite / negative `dt`) is the tests' and automation's
/// frozen frame: it always renders, and neither the gate nor the game clock
/// moves.
#[no_mangle]
pub extern "C" fn step(dt: f32) -> i32 {
    // Guard a non-finite / negative dt so both clocks only move forward.
    let real_dt = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
    let mut ran = 0;
    ensure_app(|a| {
        // Advance realtime every call, even a skipped one (Host_FilterTime's
        // `realtime += time`): it drives the flashing cursors, which keep
        // animating over a frozen frame.
        a.realtime += real_dt as f64;
        let dt = if real_dt == 0.0 {
            0.0
        } else {
            match host_filter_time(a.realtime, &mut a.oldrealtime) {
                Some(frametime) => frametime,
                None => return,
            }
        };
        ran = 1;
        bench::frame_begin();
        // host_time: the menudot spinner (mode-independent, like realtime).
        a.clock += dt;
        let (w, h) = (a.render_w, a.render_h);
        // While the menu OR console is up, gameplay input is gated; the dispatcher
        // owns that state, so it tells step_walk whether to gate. step_demo ignores
        // gameplay input regardless. The console takes priority over the menu.
        let menu_visible = a.menu.visible;
        let gate_gameplay = menu_visible || a.console.open;
        // Keep the menu's M_Menu_Save_f gate current: a local single-player game
        // is running when walk mode is live and not in intermission (`sv.active
        // && !cl.intermission && svs.maxclients == 1` — always 1 client here).
        let game_active =
            a.mode == 0 && a.walk.as_ref().map(|wk| wk.intermission == 0).unwrap_or(false);
        a.menu.set_game_active(game_active);
        // Derive this frame's bindings-driven keyboard input (CL_BaseMove over
        // keys.c's keybindings) and hand it to the walk; step_walk zeroes it
        // while gameplay is gated.
        let km = derive_key_move(&a.menu, &a.keys_held);
        let viewsize = a.menu.viewsize();
        if let Some(wk) = a.walk.as_mut() {
            wk.key_move = km;
            wk.viewsize = viewsize;
        }
        if let Some(d) = a.demo.as_mut() {
            d.viewsize = viewsize;
        }
        // Each mode returns its frame plus a DEFERRED screen blend (color, alpha):
        // the software V_UpdatePalette cshift tints the WHOLE screen, so we apply it
        // after the HUD/menu/console have composited, not just over the 3D view.
        bench::lap(Phase::Input);
        let frame = if a.mode == 1 {
            a.demo.as_mut().map(|d| step_demo(d, dt, gate_gameplay, w, h))
        } else {
            a.walk.as_mut().map(|wk| step_walk(wk, dt, gate_gameplay, w, h))
        };
        let (mut img, blend) = match frame {
            Some((image, bc, ba)) => (Some(image), (bc, ba)),
            None => (None, ([0u8, 0, 0], 0.0f32)),
        };

        // svc_sellscreen (cl_parse.c): the C ran `Cmd_ExecuteString("help")` —
        // pop the Help/Ordering menu (the shareware episode-end "order Quake"
        // pitch). The walk raised the flag during its step; the menu (owned
        // here, at the App level) opens on the Help screen for the next frame.
        if let Some(wk) = a.walk.as_mut() {
            if wk.pending_sellscreen {
                wk.pending_sellscreen = false;
                a.menu.open_help();
            }
        }

        // The main menu overlays WHATEVER is playing (walk OR the attract demo).
        // Drawn here in the dispatcher, after the active mode rendered its frame
        // and BEFORE packing to the framebuffer. Mirroring Quake's key_dest model
        // (M_Draw is a no-op when key_dest == key_console), the menu is suppressed
        // while the console is down — the console owns the screen+keyboard — so the
        // two never both show (and the console sits on top, matching SCR_UpdateScreen
        // drawing SCR_DrawConsole then M_Draw under mutual exclusion).
        // Uses the active mode's palette, host_time (the App clock) for the
        // menudot spinner and realtime for the flashing cursors.
        if menu_visible && !a.console.open {
            // Keep the Video Options "current mode" tracking the actual render
            // resolution (the framebuffer is the source of truth), so a boot /
            // New Game / `map` that changed the render size can't leave it stale.
            a.menu.sync_resolution(a.render_w as i32, a.render_h as i32);
            if let Some(img) = img.as_mut() {
                if let Some(palette) = a.active_palette() {
                    render::draw_menu(
                        img,
                        &a.menu,
                        &a.menu_pics,
                        a.conchars.as_ref(),
                        a.clock,
                        a.realtime,
                        palette,
                    );
                }
            }
        }
        bench::lap(Phase::Menu);

        // The console overlays everything and (per the gate above) replaces the menu
        // while open — matching Quake, where the menu and the drop-down console are
        // mutually exclusive via key_dest. It owns the keyboard while open. Uses the
        // active mode's palette and realtime for the input cursor flash
        // (Con_DrawInput). A closed console draws nothing.
        if a.console.open {
            if let Some(img) = img.as_mut() {
                if let Some(palette) = a.active_palette() {
                    render::draw_console(
                        img,
                        &a.console,
                        a.conback.as_ref(),
                        a.conchars.as_ref(),
                        palette,
                        a.realtime,
                    );
                }
            }
        }
        bench::lap(Phase::Console);

        // V_UpdatePalette runs LAST in SCR_UpdateScreen: tint the fully composited
        // frame (3D + HUD + centerprint/notify + menu + console) with the deferred
        // damage/water/powerup blend, matching software Quake's whole-screen palette
        // shift. A zero alpha (no active shift, or the demo path) is a no-op.
        if let Some(img) = img.as_mut() {
            render::apply_blend(img, blend.0, blend.1);
        }
        bench::lap(Phase::Blend);

        if let Some(img) = img {
            // V_CheckGamma (view.c): rebuild the gamma LUT only when the cvar
            // actually changed since the last frame.
            let g = a.menu.gamma();
            if g != a.gamma_value {
                a.gamma_value = g;
                a.gamma_table = build_gamma_table(g);
            }
            let fb = &mut a.fb;
            fb.clear();
            if a.gamma_value == 1.0 {
                // BuildGammaTable's g == 1.0 identity: skip the LUT entirely so
                // the default presentation stays byte-exact.
                for px in &img.rgb {
                    fb.push(px[0]);
                    fb.push(px[1]);
                    fb.push(px[2]);
                    fb.push(255);
                }
            } else {
                // The port's hardware-palette boundary (VID_ShiftPalette): the
                // finished, cshift-blended frame maps through gammatable as it
                // becomes the presented RGBA — the same order as the C, where
                // V_UpdatePalette blends the cshifts into the palette FIRST and
                // gamma is applied to the result.
                let t = &a.gamma_table;
                for px in &img.rgb {
                    fb.push(t[px[0] as usize]);
                    fb.push(t[px[1] as usize]);
                    fb.push(t[px[2] as usize]);
                    fb.push(255);
                }
            }
        }
        bench::lap(Phase::Pack);
        bench::frame_end();
    });
    ran
}

#[cfg(test)]
#[path = "census_tests.rs"]
mod census_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::console_toggle;
    use crate::input::*;
    use crate::menu::*;
    use crate::test_util::*;

    // -- Host_FilterTime: realtime vs host_time ---------------------------------

    #[test]
    fn step_splits_realtime_from_host_time_like_host_filtertime() {
        // The page hands step() the RAW elapsed time. Like Host_FilterTime,
        // all of it goes to realtime (the flashing cursors' clock) while the
        // game clock (host_time: the menudot spinner, the world) advances by
        // host_frametime = min(dt, 0.1). No mode is booted: the clocks tick
        // regardless.
        let clocks = || {
            APP.with(|c| {
                let b = c.borrow();
                let a = b.as_ref().expect("step creates the app");
                (a.realtime, a.clock)
            })
        };
        step(0.5); // a half-second hitch
        let (rt, ht) = clocks();
        assert!((rt - 0.5).abs() < 1e-9, "realtime takes the whole hitch: {rt}");
        assert!((ht - 0.1).abs() < 1e-6, "host_time is capped at 0.1: {ht}");
        step(0.05); // a normal frame advances both alike
        let (rt, ht) = clocks();
        assert!((rt - 0.55).abs() < 1e-6 && (ht - 0.15).abs() < 1e-6, "{rt} {ht}");
        // Garbage never runs either clock backwards.
        step(f32::NAN);
        step(-1.0);
        let (rt2, ht2) = clocks();
        assert_eq!((rt2, ht2), (rt, ht), "non-finite / negative dt is ignored");
    }

    /// Drive [`host_filter_time`] the way the page does: one call per rAF
    /// timestamp (ms, as the browser reports them), `dt` narrowed to the
    /// export's f32 and summed into an f64 `realtime`. Returns, per call,
    /// `Some(host_frametime)` when a frame ran.
    fn gate_run(stamps_ms: &[f64]) -> Vec<Option<f32>> {
        let (mut realtime, mut oldrealtime, mut last) = (0.0f64, 0.0f64, 0.0f64);
        stamps_ms
            .iter()
            .map(|&now| {
                let dt = ((now - last) / 1000.0).max(0.0) as f32;
                last = now;
                realtime += dt as f64;
                host_filter_time(realtime, &mut oldrealtime)
            })
            .collect()
    }

    /// The call indices at which a frame ran.
    fn ran_at(runs: &[Option<f32>]) -> Vec<usize> {
        runs.iter().enumerate().filter(|(_, r)| r.is_some()).map(|(i, _)| i).collect()
    }

    /// A tiny deterministic LCG in [0, 1) for jittered timestamps.
    fn lcg(seed: &mut u32) -> f64 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (*seed >> 8) as f64 / (1u32 << 24) as f64
    }

    #[test]
    fn host_filter_time_caps_every_refresh_rate_at_a_steady_cadence() {
        // (display Hz, vsyncs per host frame). Browsers report rAF stamps
        // coarsened to 0.1 ms without cross-origin isolation: round to that.
        // 144 Hz is the case the tolerance exists for (2 vsyncs = 13.889 ms,
        // a hair either side of 1/72 s): a steady 72, never 72/48 judder.
        let cases = [
            (60.0, 1),  // 60 fps: every refresh
            (75.0, 1),  // 75 fps: the tolerance's one overshoot of the cap
            (90.0, 2),  // 45 fps
            (100.0, 2), // 50 fps
            (120.0, 2), // 60 fps
            (144.0, 2), // 72 fps
            (165.0, 3), // 55 fps: 2 vsyncs (12.12 ms) are still too fast
            (240.0, 4), // 60 fps: 3 vsyncs (12.5 ms) are still too fast
            (360.0, 5), // 72 fps
        ];
        for (hz, k) in cases {
            let stamps: Vec<f64> = (1..=(hz as usize) * 10)
                .map(|i| ((i as f64 * 1000.0 / hz) * 10.0).round() / 10.0)
                .collect();
            let runs = gate_run(&stamps);
            let at = ran_at(&runs);
            assert_eq!(at[0], k - 1, "{hz} Hz: the first frame runs after {k} vsyncs");
            assert!(
                at.windows(2).all(|w| w[1] - w[0] == k),
                "{hz} Hz: every host frame is exactly {k} vsyncs apart"
            );
            let fps = at.len() as f64 / 10.0;
            assert!((fps - hz / k as f64).abs() < 0.2, "{hz} Hz: {fps} fps");
            // The game clock loses nothing: host_frametime sums to real time.
            let game: f64 = runs.iter().flatten().map(|&t| t as f64).sum();
            let real = stamps[*at.last().unwrap()] / 1000.0;
            assert!((game - real).abs() < 1e-3, "{hz} Hz: game {game} vs real {real}");
        }
    }

    #[test]
    fn host_filter_time_holds_its_cadence_through_jitter_and_dropped_frames() {
        let mut seed = 0x5eed_u32;
        // 144 Hz with +-0.3 ms of timestamp jitter, then 0.1 ms coarsening:
        // still exactly every other refresh.
        let stamps: Vec<f64> = (1..=1440)
            .map(|i| {
                let t = i as f64 * 1000.0 / 144.0 + (lcg(&mut seed) - 0.5) * 0.6;
                (t * 10.0).round() / 10.0
            })
            .collect();
        let at = ran_at(&gate_run(&stamps));
        assert!(at.windows(2).all(|w| w[1] - w[0] == 2), "jittered 144 Hz stays at 72 fps");
        assert_eq!(at.len(), 720);

        // 60 Hz dropping every 7th refresh (a slow frame): every call runs,
        // and the long ones carry their whole 33 ms into the game.
        let mut stamps = Vec::new();
        let mut t = 0.0;
        for i in 1..=600 {
            t += if i % 7 == 0 { 2000.0 / 60.0 } else { 1000.0 / 60.0 };
            stamps.push(t);
        }
        let runs = gate_run(&stamps);
        assert!(runs.iter().all(|r| r.is_some()), "60 Hz never skips a refresh");
        assert!(runs.iter().flatten().any(|&f| (f - 2.0 / 60.0).abs() < 1e-4));

        // Irregular intervals (2..40 ms): a frame runs as soon as 1/72 s minus
        // the tolerance has passed since the last one, never earlier, and the
        // game clock (no interval reaches the 0.1 s clamp) matches real time.
        let mut stamps = Vec::new();
        let mut t = 0.0;
        for _ in 0..2000 {
            t += 2.0 + 38.0 * lcg(&mut seed);
            stamps.push(t);
        }
        let runs = gate_run(&stamps);
        let at = ran_at(&runs);
        let min = (HOST_FRAME_INTERVAL - HOST_FRAME_TOLERANCE) * 1000.0;
        let mut prev = 0.0;
        for (i, &now) in stamps.iter().enumerate() {
            let since = now - prev;
            // 1e-3 ms of slack for the f32 narrowing of each dt.
            if runs[i].is_some() {
                assert!(since >= min - 1e-3, "call {i} ran {since} ms after the last frame");
                prev = now;
            } else {
                assert!(since < min + 1e-3, "call {i} skipped {since} ms after the last frame");
            }
        }
        let game: f64 = runs.iter().flatten().map(|&t| t as f64).sum();
        assert!((game - stamps[*at.last().unwrap()] / 1000.0).abs() < 1e-3);
    }

    #[test]
    fn step_runs_72_frames_a_second_on_a_144hz_display_and_dt_zero_still_freezes() {
        assert_eq!(boot(), 1);
        close_menu();
        let clocks = || {
            APP.with(|c| {
                let b = c.borrow();
                let a = b.as_ref().unwrap();
                (a.clock, a.realtime, a.walk.as_ref().unwrap().clock)
            })
        };
        let (host0, real0, walk0) = clocks();
        let vsync = 1.0f32 / 144.0;
        let mut ran = 0;
        for _ in 0..144 {
            let before = clocks();
            let r = step(vsync);
            let after = clocks();
            if r == 0 {
                // A skipped call: realtime moves, nothing else does.
                assert_eq!((before.0, before.2), (after.0, after.2), "skipped call moved the game");
                assert!(after.1 > before.1, "realtime runs on every call");
            }
            ran += r;
        }
        assert_eq!(ran, 72, "one second at 144 Hz is 72 host frames");
        let (host1, real1, walk1) = clocks();
        assert!((real1 - real0 - 1.0).abs() < 1e-4, "realtime took the whole second");
        assert!((host1 - host0 - 1.0).abs() < 1e-3, "host_time lost nothing: {}", host1 - host0);
        assert!((walk1 - walk0 - 1.0).abs() < 1e-3, "the walk advanced 1 s: {}", walk1 - walk0);
        // dt = 0 is the automation's frozen frame: it always renders, and no
        // clock moves.
        assert_eq!(step(0.0), 1);
        assert_eq!(step(0.0), 1);
        assert_eq!(clocks(), (host1, real1, walk1));
    }

    #[test]
    fn options_and_rebinds_survive_reboot_and_new_game() {
        // The promotion of the menu to the authoritative store for bindings +
        // live cvars means a re-boot must NOT wipe them (WinQuake's `map start`
        // never resets cvars or keybindings — they're host state). Drive the
        // real export paths the page uses.
        reset_queue();
        assert_eq!(boot(), 1); // opens the menu on Main, cursor 0
        menu_down();
        menu_down();
        menu_select(); // Main row 2 -> Options (cursor 0 = Customize controls)
        for _ in 0..4 {
            menu_down();
        }
        menu_right(); // Brightness row: v_gamma 1.0 -> 0.95
        for _ in 0..4 {
            menu_down();
        }
        menu_right(); // Always Run row: toggle OFF (this port defaults it on)
        for _ in 0..8 {
            menu_up();
        }
        menu_select(); // Customize controls -> Keys screen
        menu_down();
        menu_down(); // "jump / swim up" row
        menu_select(); // starts the bind grab
        menu_bind_key(i32::from(b'j'));
        APP.with(|c| {
            let b = c.borrow();
            let m = &b.as_ref().unwrap().menu;
            assert!((m.gamma() - 0.95).abs() < 1e-6, "gamma set through the menu");
            assert!(!m.always_run(), "Always Run toggled off through the menu");
            assert_eq!(m.action_for_key(b'j'), Some(render::BIND_JUMP), "rebound");
        });

        // Re-boot the walk (the page's walk button): navigation comes back
        // fresh (open, Main, cursor 0) but every user choice survives.
        assert_eq!(boot(), 1);
        APP.with(|c| {
            let b = c.borrow();
            let m = &b.as_ref().unwrap().menu;
            assert!(m.visible, "boot reopens the menu");
            assert_eq!(m.screen(), render::MenuScreen::Main, "navigation reset");
            assert_eq!(m.cursor(), 0, "cursor reset");
            assert!((m.gamma() - 0.95).abs() < 1e-6, "Brightness survives re-boot");
            assert!(!m.always_run(), "Always Run (toggled off) survives re-boot");
            assert_eq!(
                m.action_for_key(b'j'),
                Some(render::BIND_JUMP),
                "key rebind survives re-boot"
            );
        });

        // The flagship flow: Single Player > New Game keeps them too.
        menu_select(); // Main > Single Player
        menu_select(); // New Game -> fresh walk on the start hub, menu closed
        assert_eq!(menu_visible(), 0, "New Game closes the menu");
        APP.with(|c| {
            let b = c.borrow();
            let m = &b.as_ref().unwrap().menu;
            assert!((m.gamma() - 0.95).abs() < 1e-6, "Brightness survives New Game");
            assert!(!m.always_run(), "Always Run (toggled off) survives New Game");
            assert_eq!(
                m.action_for_key(b'j'),
                Some(render::BIND_JUMP),
                "key rebind survives New Game"
            );
        });
        // And the surviving choice is LIVE in the fresh walk: with Always Run
        // toggled off, +forward walks at cl_forwardspeed 200 (the 200<->400
        // swap), not the on-by-default 400.
        key_down(i32::from(b'w'));
        step(0.05);
        assert_eq!(
            walk_mut(|w| w.key_move.fwd),
            200.0,
            "Always Run off drives the new walk at 200"
        );
        key_up(i32::from(b'w'));
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
    fn in_walk_mode_tracks_every_mode_transition() {
        // The page's pointer-lock gating + "click to capture mouse" chip key off
        // this export: only the live walk wants the mouse captured. It must track
        // every mode transition, including the engine-internal New Game path the
        // page cannot otherwise observe.
        assert_eq!(boot_attract(), 1);
        assert_eq!(in_walk_mode(), 0, "attract loop = demo playback, not a walk");

        // Menu-driven New Game (Main > Single Player > New Game), the path the
        // page only sees as two opaque menu_select() calls.
        menu_select();
        menu_select();
        assert_eq!(in_walk_mode(), 1, "New Game from the attract menu enters walk mode");

        assert_eq!(boot_demo(), 1);
        assert_eq!(in_walk_mode(), 0, "demo playback leaves walk mode");

        assert_eq!(boot(), 1);
        assert_eq!(in_walk_mode(), 1, "the walk button re-enters walk mode");
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
            let (plain, _, _) = step_demo(d, 0.0001, false, w, h);
            let (mut withm, _, _) = step_demo(d, 0.0001, false, w, h);
            let pal = a.active_palette().expect("demo palette");
            render::draw_menu(&mut withm, &a.menu, &a.menu_pics, a.conchars.as_ref(), a.clock, a.realtime, pal);
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

    #[test]
    fn gamma_changes_the_presented_frame_and_one_is_byte_identity() {
        reset_queue();
        assert_eq!(boot(), 1);
        close_menu();
        let grab = || APP.with(|c| c.borrow().as_ref().unwrap().fb.clone());
        // dt=0 keeps the world/clock frozen, so back-to-back frames are
        // byte-identical and the ONLY variable below is the gamma LUT.
        step(0.0);
        let base = grab();
        step(0.0);
        assert_eq!(base, grab(), "dt=0 frames are deterministic");

        // Brightness right one notch (Options row 4): gamma 1.0 -> 0.95
        // (v_gamma.value -= dir * 0.05) — the presented bytes must change.
        menu_cancel();
        menu_down();
        menu_down();
        menu_select(); // -> Options
        for _ in 0..4 {
            menu_down(); // ROW_BRIGHTNESS
        }
        menu_right();
        menu_cancel();
        menu_cancel();
        assert_eq!(menu_visible(), 0);
        step(0.0);
        let bright = grab();
        assert_ne!(base, bright, "gamma 0.95 changes the presented frame");
        // No pixel got darker (the curve brightens everything below white).
        assert!(
            base.iter().zip(bright.iter()).all(|(a, b)| b >= a),
            "gamma < 1 must only brighten"
        );

        // Back to 1.0: the identity special case restores the EXACT bytes.
        menu_cancel(); // reopen (lands on Main)
        menu_down();
        menu_down();
        menu_select();
        for _ in 0..4 {
            menu_down();
        }
        menu_left(); // gamma 0.95 -> 1.0 (clamped at GAMMA_MAX)
        menu_cancel();
        menu_cancel();
        step(0.0);
        assert_eq!(base, grab(), "gamma 1.0 is a byte-exact identity");
    }
}

