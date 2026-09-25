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
    self, build_gamma_table, Camera, Console, Menu, MenuPics, ModelInstance, Viewmodel,
};
use quake_rs::server::{Server, UserCmd};
use quake_rs::tent::{BeamModel, BeamSegment, Beams};
use quake_rs::wad::Qpic;

mod bench;
use bench::Phase;
mod cl_tent;
mod console;
mod host_cmd;
mod input;
mod menu;
mod savegame;
mod snd_dma;
mod vid;
#[cfg(test)]
mod test_util;

use cl_tent::{rocket_trail_type, spawn_temp_entity};
use host_cmd::{try_changelevel, try_restart, FL_ONGROUND, IT_INVISIBILITY};
use input::{
    clamp_pitch, derive_key_move, KeyMove, CL_ANGLESPEEDKEY, CL_PITCHSPEED, CL_YAWSPEED, SPEED,
    V_CENTERSPEED,
};
use vid::{backtile_for, DEFAULT_H, DEFAULT_W};
use snd_dma::{
    bump_sound_generation, push_stop_sounds, queue_sounds, queue_static_sounds,
    update_ambient_channels, Listener, LISTENER, SND_QUEUE, STOP_SND_QUEUE,
};

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

/// Owned visible-entity descriptor gathered from the server before rendering:
/// `(model name, origin, angles, frame, shirt/pants colour, skin)`.
type EntityDesc = (String, [f32; 3], [f32; 3], usize, [u8; 3], i32);

fn step_walk(
    w: &mut Walk,
    dt: f32,
    menu_up: bool,
    render_w: usize,
    render_h: usize,
) -> (render::Image, [u8; 3], f32) {
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
    // The bindings-driven keyboard input `step` derived this frame; zeroed while
    // the menu/console gate gameplay (key_dest != key_game).
    let km = if menu_up { KeyMove::default() } else { w.key_move };

    // CL_AdjustAngles (cl_input.c), run before CL_BaseMove builds the cmd like
    // CL_SendMove does: the keyboard turn/look keys move the view angles at
    // cl_yawspeed/cl_pitchspeed deg/sec (x cl_anglespeedkey with +speed held).
    if dt.is_finite() && dt > 0.0 {
        let aspeed = dt * if km.speed { CL_ANGLESPEEDKEY } else { 1.0 };
        w.yaw += aspeed * CL_YAWSPEED * km.turn;
        if km.look != 0.0 {
            // PITCH -= speed*cl_pitchspeed*up (look up = pitch down numerically);
            // "if (up || down) V_StopPitchDrift()"; clamp 80/-70 (clamp_pitch).
            w.pitch = clamp_pitch(w.pitch - aspeed * CL_PITCHSPEED * km.look);
            w.pitch_drift = false;
            w.pitch_vel = 0.0;
        }
        // V_DriftPitch (view.c), the active-drift arm: centerview / a lookspring
        // pointer-unlock seeded pitch_vel and the view re-levels toward the
        // ideal pitch. SIMPLIFICATION: cl.idealpitch is fixed at 0 here (the C
        // computes a walking idealpitch from ground slope; this port never
        // does, so level is the only ideal) and the C's nodrift/driftmove
        // re-arm bookkeeping is unneeded — drifting starts only at the two
        // explicit triggers. The velocity integration matches the C: move =
        // frametime*pitchvel, pitchvel += frametime*v_centerspeed, overshoot
        // clamps to the target and stops.
        if w.pitch_drift && !menu_up {
            let delta = -w.pitch; // idealpitch (0) - viewangles[PITCH]
            if delta == 0.0 {
                w.pitch_vel = 0.0;
                w.pitch_drift = false;
            } else {
                let mut mv = dt * w.pitch_vel;
                w.pitch_vel += dt * V_CENTERSPEED;
                if mv > delta.abs() {
                    mv = delta.abs();
                    w.pitch_vel = 0.0;
                    w.pitch_drift = false;
                }
                w.pitch += mv * delta.signum();
            }
        }
    }

    // The legacy analog set_move fractions (tests/automation), normalised so a
    // diagonal can't exceed 1, at the old fixed 320 scale.
    let (mut fwd, mut side) = if menu_up { (0.0, 0.0) } else { (w.in_fwd, w.in_side) };
    let mag = (fwd * fwd + side * side).sqrt();
    if mag > 1.0 {
        fwd /= mag;
        side /= mag;
    }
    // IN_MouseMove's accumulated sidemove/forwardmove contributions (lookstrafe
    // / +strafe routing). Take them even when gated so a stale accumulation
    // can't fire after the menu closes (mouse_move is gated too, so these are
    // zero behind an overlay anyway).
    let mouse_side = std::mem::take(&mut w.mouse_side);
    let mouse_fwd = std::mem::take(&mut w.mouse_fwd);
    let cmd = UserCmd {
        // CL_BaseMove composition: keyboard (km, real cl_* cvar speeds) +
        // mouse strafe units + the legacy analog path. The server's
        // SV_AirMove clamps wishspeed to sv_maxspeed exactly like the C.
        forwardmove: fwd * SPEED + km.fwd + if menu_up { 0.0 } else { mouse_fwd },
        sidemove: side * SPEED + km.side + if menu_up { 0.0 } else { mouse_side },
        // Vertical swim intent: Space (jump) = up, c (movedown) = down. Quake's
        // SV_WaterMove consumes upmove while waist-deep; the ground/air move
        // ignores it, so on land Space still just jumps and c does nothing.
        // The legacy set_jump/set_movedown booleans keep their old 320 scale;
        // the key path contributes at cl_upspeed via km.up.
        upmove: km.up
            + if menu_up {
                0.0
            } else {
                ((if w.in_jump { 1.0 } else { 0.0 }) - (if w.in_down { 1.0 } else { 0.0 }))
                    * SPEED
            },
        yaw: w.yaw,
        pitch: w.pitch,
        buttons: if menu_up {
            0
        } else {
            (if w.in_attack || km.attack { 1 } else { 0 })
                | (if w.in_jump || km.jump { 2 } else { 0 })
        },
        impulse: if menu_up { 0 } else { w.next_impulse },
    };
    // A queued impulse fires once (the server also clears the edict field after
    // ImpulseCommands, but clearing here guarantees a held key fires a single
    // weapon switch rather than re-selecting every frame).
    w.next_impulse = 0;
    let _ = w.server.client_frame(&cmd, dt);

    // 1a. MSG_ALL server commands (CL_ParseServerMessage, cl_parse.c): the QuakeC
    //     end-of-level chain WriteBytes svc_intermission / svc_finale (+ text) /
    //     svc_sellscreen to every client; play the client role here — enter
    //     intermission mode, latch cl.completed_time, start the finale reveal.
    for ev in w.server.drain_svc_events() {
        match ev {
            quake_rs::server::SvcEvent::Intermission => {
                // cl.intermission = 1; cl.completed_time = cl.time (cl_parse.c:939).
                // On a local server cl.time tracks sv.time, which SV_SpawnServer
                // starts at 1.0 — NOT this walk's clock (which starts at 0), so the
                // overlay's minutes:seconds shows exactly what vanilla shows. (The
                // demo parser latches mtime[0], also server time — the paths agree.)
                w.intermission = 1;
                w.completed_time = w.server.time();
            }
            quake_rs::server::SvcEvent::Finale(text) => {
                // cl.intermission = 2 + SCR_CenterPrint (scr_centertime_start).
                // completed_time = cl.time = sv.time, as above; finale_start stays
                // in the walk clock — the reveal only uses the DIFFERENCE
                // w.clock - finale_start (cl.time - scr_centertime_start in the C).
                w.intermission = 2;
                w.completed_time = w.server.time();
                w.finale_text = text;
                w.finale_start = w.clock;
            }
            quake_rs::server::SvcEvent::Cutscene(text) => {
                // cl.intermission = 3 (text only, no plaque); times as per Finale.
                w.intermission = 3;
                w.completed_time = w.server.time();
                w.finale_text = text;
                w.finale_start = w.clock;
            }
            quake_rs::server::SvcEvent::SellScreen => {
                // Cmd_ExecuteString("help"): the dispatcher opens the Help menu.
                w.pending_sellscreen = true;
            }
        }
    }

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
    } else if w.server.take_pending_restart() {
        // Single-player respawn: QuakeC ran localcmd("restart") (a dead player who
        // pressed a button). Reload the current level with the entry inventory.
        // `else if` so a changelevel this frame takes precedence over a restart.
        try_restart(w);
    }

    // 2. Surface the sounds the world fired this frame (gunshots, doors, monster
    //    voices) to the page's audio queue.
    let events = w.server.drain_sounds();
    queue_sounds(&w.pak, &events, w.player);

    // 2a. Drain QuakeC's on-screen messages (centerprint / sprint / bprint) into
    //     the timed display state, and expire old ones (clock = w.clock).
    for m in w.server.drain_messages() {
        if m.center {
            w.centerprint = Some((m.text, w.clock + 2.0));
        } else {
            // Con_Print model: accumulate notify text and only break into a line on
            // '\n'. Quake pickups print via several sprint() calls ("You receive ",
            // "25", " health\n") that the C console joins into ONE line; emitting one
            // notify line per call would wrongly split a single message across lines.
            w.notify_pending.push_str(&m.text);
        }
    }
    // Flush every complete ('\n'-terminated) line from the pending buffer; the
    // trailing partial (no newline yet) stays buffered until more text arrives.
    while let Some(nl) = w.notify_pending.find('\n') {
        let line: String = w.notify_pending.drain(..=nl).collect();
        let line = line.trim_end_matches(['\n', '\r']).to_string();
        if !line.trim().is_empty() {
            w.notify.push((line, w.clock + 3.0));
            while w.notify.len() > 4 {
                w.notify.remove(0);
            }
        }
    }
    if let Some((_, exp)) = &w.centerprint {
        if w.clock >= *exp {
            w.centerprint = None;
        }
    }
    let clock = w.clock;
    w.notify.retain(|(_, exp)| clock < *exp);

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
        // Beam types (CL_ParseTEnt's TE_LIGHTNING1/2/3 + TE_BEAM cases): refresh
        // the entity's beam slot (CL_ParseBeam) and load its bolt model now —
        // the C's `CL_ParseBeam(Mod_ForName("progs/bolt*.mdl", true))` loads at
        // parse time too. A missing model (shareware lacks beam.mdl) caches
        // `None` and the expansion below skips its pieces (the C Sys_Error'd;
        // vanilla progs never emits TE_BEAM, so the path was never live).
        if let Some(bm) = BeamModel::from_te_type(ev.te_type) {
            w.beams.parse_beam(ev.entity, bm, ev.pos, ev.end, now);
            let name = bm.model_name();
            if !w.model_cache.contains_key(name) {
                let parsed =
                    w.pak.read_file(name).ok().flatten().and_then(|b| Mdl::parse(&b).ok());
                w.model_cache.insert(name.to_string(), parsed);
            }
            continue; // beams spawn no particles / sounds / dlights here
        }
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
        } else if m.ends_with(".spr") && !w.sprite_cache.contains_key(&m) {
            // A sprite-model entity (progs/s_explod.spr explosion flash, bubbles).
            // Parse once and cache; None on missing/unparseable.
            let parsed =
                w.pak.read_file(&m).ok().flatten().and_then(|b| quake_rs::spr::Sprite::parse(&b).ok());
            w.sprite_cache.insert(m, parsed);
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
    // (model name, origin, angles, frame, shirt/pants colour, skin) per entity.
    let mut descs: Vec<EntityDesc> = Vec::new();
    let mut bmodels: Vec<render::BModelInstance> = Vec::new();
    // Projectile/gib trails to spawn this frame, collected here and emitted after
    // the loop (so we don't borrow w.particles/dlights while reading the server):
    // (entity, old origin, new origin, R_RocketTrail type).
    let mut trail_spawns: Vec<(i32, [f32; 3], [f32; 3], i32)> = Vec::new();
    // External brush-model items (maps/b_*.bsp) as owned (name, origin) pairs; the
    // borrowing `ExternalBModel` list is built below, after the cache is final, so
    // the immutable cache borrow does not clash with reading the server here.
    let mut ext_descs: Vec<(String, [f32; 3])> = Vec::new();
    // Sprite-model entities (name, origin, frame): the explosion flash, bubbles.
    // Resolved against the sprite cache after the loop (disjoint borrows).
    let mut sprite_descs: Vec<(String, [f32; 3], usize)> = Vec::new();
    // Drop trail history for any edict that is currently free. When `ED_Free`
    // recycles a slot for a new trailed entity (rocket/grenade/gib), a stale
    // `trail_org[ent]` from the previous occupant would make R_RocketTrail draw a
    // spurious streak from the old entity's last origin to the new spawn point.
    // Pruning here lets a reused slot start fresh (oldorg defaults to its own
    // origin below, so no trail on the first frame). Disjoint field borrows.
    {
        let vm = &w.server.vm;
        w.trail_org
            .retain(|&e, _| !vm.edict_free.get(e as usize).copied().unwrap_or(true));
    }
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
                // The entity's `frame` selects the alternate (+a..+j) texture cycle
                // for activated buttons/doors (a pressed button shows its lit face).
                let frame = w.server.vm.ent_get_float(ent, "frame") as i32;
                bmodels.push(render::BModelInstance { model_index: idx, origin, frame });
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
        // Sprite-model entities (s_explod.spr explosion flash, bubbles): a camera-
        // facing billboard at the entity origin, current `frame` for the animation.
        if m.ends_with(".spr") {
            let origin = w.server.vm.ent_get_vector(ent, "origin");
            let frame = w.server.vm.ent_get_float(ent, "frame").max(0.0) as usize;
            sprite_descs.push((m, origin, frame));
            continue;
        }
        if !m.ends_with(".mdl") {
            continue;
        }
        let origin = w.server.vm.ent_get_vector(ent, "origin");
        let frame = w.server.vm.ent_get_float(ent, "frame").max(0.0) as usize;
        let color = color_for_name(&m);
        // The model header flags (rocket/grenade/gib/tracer trails + EF_ROTATE).
        let mflags = w
            .model_cache
            .get(&m)
            .and_then(|o| o.as_ref())
            .map(|md| md.header.flags)
            .unwrap_or(0);
        // CL_RelinkEntities (cl_main.c:531): a model carrying EF_ROTATE (bonus
        // pickups — ammo/health/armour boxes, weapons, keys, runes, powerups) has
        // its yaw overwritten with `anglemod(100*cl.time)` every frame so it spins.
        // Otherwise use the entity's own yaw. Without this every pickup sat frozen.
        let ent_angles = w.server.vm.ent_get_vector(ent, "angles");
        let yaw = if mflags & quake_rs::demo::EF_ROTATE != 0 {
            quake_rs::demo::rotate_yaw(w.clock)
        } else {
            ent_angles[1]
        };
        // [pitch, yaw, roll]: EF_ROTATE overrides yaw only; pitch/roll come straight
        // from the entity so flying projectiles point along their flight path
        // (r_alias.c R_AliasSetUpTransform), not just spin about Z.
        let angles = [ent_angles[0], yaw, ent_angles[2]];
        // Per-entity skin index (R_AliasSetupSkin: `skinnum = currententity->skinnum`).
        // Drives e.g. armor.mdl's 3 skins (green/yellow/red); was hardcoded to 0.
        let skin = w.server.vm.ent_get_float(ent, "skin").max(0.0) as i32;
        // R_RocketTrail: a model with a rocket/grenade/gib/tracer header flag
        // trails particles from its previous origin to here (CL_RelinkEntities).
        if let Some(ttype) = rocket_trail_type(mflags) {
            let oldorg = *w.trail_org.get(&ent).unwrap_or(&origin);
            trail_spawns.push((ent, oldorg, origin, ttype));
            w.trail_org.insert(ent, origin);
        }
        descs.push((m, origin, angles, frame, color, skin));
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
    //    height (V_CalcBob) so the view rocks as the player moves. During an
    //    intermission the refdef is V_CalcIntermissionRefdef (view.c) instead:
    //    the QuakeC moved the player entity to the info_intermission spot, so
    //    the camera is the RAW entity origin + angles — no view_ofs, no bob, no
    //    punch, no strafe/death roll — plus the forced v_idlescale=1 sway of
    //    V_AddIdle (the gentle drift id's intermission camera has).
    let intermission = w.intermission != 0;
    let (mut eye, ang) = if intermission {
        // ent->origin / ent->angles: the QC set `angles = pos.mangle` (fixangle)
        // and froze the player MOVETYPE_NONE, which SV_ClientThink early-outs on,
        // so the spot's angles survive the per-frame mouse v_angle updates.
        (
            w.server.vm.ent_get_vector(w.player, "origin"),
            w.server.vm.ent_get_vector(w.player, "angles"),
        )
    } else {
        w.server.player_view()
    };
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

    // S_UpdateAmbientSounds: ramp the four automatic ambient channels toward
    // the VIEW leaf's ambient_level[] targets (water wash / sky wind). Uses the
    // same steady (un-bobbed) eye as the listener pose above.
    update_ambient_channels(&w.bsp, eye, dt);

    // Bob the rendered eye only (the listener pose above stays steady so audio
    // panning does not jitter with the head-bob). Skipped during intermission
    // (V_CalcIntermissionRefdef has no bob and no stair smoothing).
    if !intermission {
        eye[2] += bob;
        // Stair-step view smoothing (view.c V_CalcRefdef ~960): while on the ground
        // and the player's origin Z rose this frame, lag the eye Z behind by up to 12
        // units and catch up at 80 u/s, so climbing stairs glides instead of jolting
        // up each 16/18-unit step. The delta is relative to the raw origin Z (bob
        // layered on top); on first frame / not-climbing, oldz tracks origin exactly.
        let origin_z = w.server.vm.ent_get_vector(w.player, "origin")[2];
        let onground = (w.server.vm.ent_get_float(w.player, "flags") as i32) & FL_ONGROUND != 0;
        if w.oldz.is_finite() && onground && origin_z - w.oldz > 0.0 {
            w.oldz += dt.max(0.0) * 80.0;
            if w.oldz > origin_z {
                w.oldz = origin_z;
            }
            if origin_z - w.oldz > 12.0 {
                w.oldz = origin_z - 12.0;
            }
            eye[2] += w.oldz - origin_z;
        } else {
            w.oldz = origin_z;
        }
    }
    let cam = if intermission {
        // V_AddIdle with v_idlescale forced to 1 (view.c V_CalcIntermissionRefdef):
        // angle += sin(cl.time * v_i*_cycle) * v_i*_level, with the stock cvar
        // defaults — roll 0.5/0.1, pitch 1/0.3, yaw 2/0.3.
        Camera {
            pos: eye,
            yaw: ang[1] + (w.clock * 2.0).sin() * 0.3,
            // QuakeC pitch is +down; the renderer's is +up.
            pitch: -(ang[0] + (w.clock * 1.0).sin() * 0.3),
            roll: ang[2] + (w.clock * 0.5).sin() * 0.1,
            fov_deg: 90.0,
        }
    } else {
        // Add the weapon-fire view kick (cl.punchangle, view.c:957); the engine's
        // drop_punch_angle already decays it back to zero each frame.
        let punch = w.server.vm.ent_get_vector(w.player, "punchangle");
        // View bank (V_CalcViewRoll, view.c:808): strafe lean from side-velocity,
        // plus the punchangle's roll component; the dead-view tilt (80°) overrides
        // when the player is dead. (Damage-kick roll needs svc_damage, not wired.)
        let body_angles = w.server.vm.ent_get_vector(w.player, "angles");
        let mut roll = quake_rs::server::v_calc_roll(body_angles, vel) + punch[2];
        if w.server.vm.ent_get_float(w.player, "health") <= 0.0 {
            roll = 80.0; // dead view angle (replaces, per V_CalcViewRoll)
        }
        Camera {
            pos: eye,
            yaw: ang[1] + punch[1],
            pitch: -(ang[0] + punch[0]), // QuakeC pitch is +down; the renderer's is +up.
            roll,
            fov_deg: 90.0,
        }
    };
    let mut instances: Vec<ModelInstance> = descs
        .iter()
        .filter_map(|(name, origin, angles, frame, color, skin)| match w.model_cache.get(name) {
            Some(Some(mdl)) => Some(ModelInstance {
                mdl,
                origin: *origin,
                yaw: angles[1],
                pitch: angles[0],
                roll: angles[2],
                color: *color,
                frame: *frame,
                skinnum: *skin,
            }),
            _ => None,
        })
        .collect();
    // CL_UpdateTEnts (cl_tent.c): expand every live lightning beam into one
    // bolt-model piece every 30 units (shared integer pitch/yaw, random roll
    // per piece per frame). A beam owned by the view entity (the player's
    // thunderbolt) is re-anchored to the player's CURRENT origin first. Gated
    // on any_live so the common no-beam frame pays one boolean scan.
    if w.beams.any_live(now) {
        let player_org = w.server.vm.ent_get_vector(w.player, "origin");
        w.beams.update(now, w.player, player_org, &mut w.prng, &mut w.beam_scratch);
        for seg in &w.beam_scratch {
            // CL_NewTempEntity memsets the entity: frame 0, skin 0. A `None`
            // cache entry (model absent from the pak) skips the piece.
            if let Some(Some(mdl)) = w.model_cache.get(seg.model.model_name()) {
                instances.push(ModelInstance {
                    mdl,
                    origin: seg.origin,
                    yaw: seg.yaw,
                    pitch: seg.pitch,
                    roll: seg.roll,
                    color: color_for_name(seg.model.model_name()),
                    frame: 0,
                    skinnum: 0,
                });
            }
        }
    }
    // External brush-model item boxes: resolve each (name, origin) against the
    // bmodel cache, dropping any box whose bsp was missing/unparseable (`None`).
    let external: Vec<render::ExternalBModel> = ext_descs
        .iter()
        .filter_map(|(name, origin)| match w.bmodel_cache.get(name) {
            Some(Some(bsp)) => Some(render::ExternalBModel { bsp, origin: *origin }),
            _ => None,
        })
        .collect();
    // Sprite-model entities: resolve each (name, origin, frame) against the sprite
    // cache, dropping any whose .spr was missing/unparseable.
    let sprites: Vec<render::SpriteInstance> = sprite_descs
        .iter()
        .filter_map(|(name, origin, frame)| match w.sprite_cache.get(name) {
            Some(Some(spr)) => Some(render::SpriteInstance { sprite: spr, origin: *origin, frame: *frame }),
            _ => None,
        })
        .collect();
    // Anchor the weapon viewmodel to the camera (drawn last, on top of the world).
    // R_DrawViewModel (r_main.c ~622) returns early — drawing NO gun — when the
    // player is dead (STAT_HEALTH <= 0) or carrying the Ring of Shadows
    // (IT_INVISIBILITY). Without this the gun hovers, frozen, on the rolled
    // death-cam, and stays visible while invisible. The intermission camera also
    // hides it (V_CalcIntermissionRefdef: `view->model = NULL`).
    let hide_gun = intermission
        || w.server.vm.ent_get_float(w.player, "health") <= 0.0
        || (w.server.vm.ent_get_float(w.player, "items") as i32) & IT_INVISIBILITY != 0;
    let viewmodel = if hide_gun {
        None
    } else {
        match w.model_cache.get(&weapon_name) {
            // V_CalcRefdef's gun origin (the forward bob + the viewsize fudge)
            // and CalcGunAngle's angles (the view before the punch, no lean).
            Some(Some(mdl)) => {
                let punch = w.server.vm.ent_get_vector(w.player, "punchangle");
                let angles = render::viewmodel_angles(&cam, punch, ang[2]);
                Some(Viewmodel {
                    mdl,
                    frame: weapon_frame,
                    origin_ofs: render::viewmodel_origin_ofs(angles, bob, w.viewsize),
                    angles,
                })
            }
            _ => None,
        }
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
    // SCR_CalcRefdef / R_SetVrect: the viewsize picks the 3-D view rectangle
    // (the view sits ABOVE the status bar, projected about its own centre) and
    // how much status bar shows; an intermission is always full screen.
    bench::lap(Phase::Sim);
    let refdef = render::calc_refdef(render_w, render_h, w.viewsize, intermission);
    let vrect = refdef.vrect;
    let mut view =
        render::render_scene_ext_sprited(&w.bsp, &cam, vrect.w, vrect.h, &w.palette, &instances, &bmodels, &external, viewmodel, w.clock, &parts, &active_dlights, &light_styles, w.colormap.as_deref(), &sprites);
    bench::lap(Phase::Render3d);

    // 5b. Screen blends (V_CalcBlend): fade the damage flash, bump it when the
    //     player lost health/armour this frame, and tint the view when the eye is
    //     under water / in lava or slime. The blend is DEFERRED (returned to the
    //     dispatcher) and applied to the whole composited frame last, matching
    //     software V_UpdatePalette's whole-screen palette shift (it tints the HUD,
    //     menu and console too — not the GL 3D-viewport-only behaviour).
    w.damage_blend = (w.damage_blend - dt * 150.0).max(0.0);
    let health = w.server.vm.ent_get_float(w.player, "health");
    let armorv = w.server.vm.ent_get_float(w.player, "armorvalue");
    if w.last_health.is_finite() {
        // V_ParseDamage (view.c:316-379): blood = health lost, armor = armour lost.
        // count = (blood+armor)/2 with a min-10 floor, and the flash adds 3*count.
        // (The C reads the server's dmg_take/dmg_save bytes; we infer them from the
        // per-frame stat deltas, which equal blood/armor in single-player.)
        let blood = (w.last_health - health).max(0.0);
        let armor = (w.last_armor - armorv).max(0.0);
        // Suppress the inferred flash during megahealth rot: above max_health the
        // QuakeC ticks health down 1/sec, which is NOT damage and never flashes in
        // id (the real CSHIFT_DAMAGE comes only from svc_damage / T_Damage). Gate on
        // post-tick health still exceeding max_health so the rot can't masquerade as
        // a hit. (A genuine hit while overhealed is rare and self-corrects next hit.)
        let max_health = w.server.vm.ent_get_float(w.player, "max_health");
        let is_rot = max_health > 0.0 && health > max_health;
        if blood + armor > 0.0 && !is_rot {
            let count = (0.5 * (blood + armor)).max(10.0);
            w.damage_blend = (w.damage_blend + 3.0 * count).min(150.0);
            // Tint: armour-dominant -> pinkish, armour-only -> orange-red, else red.
            w.damage_color = if armor > blood {
                [200, 100, 100]
            } else if armor > 0.0 {
                [220, 50, 50]
            } else {
                [255, 0, 0]
            };
        }
    }
    w.last_health = health;
    w.last_armor = armorv;
    // V_CalcBlend order: CONTENTS (bottom) -> DAMAGE -> POWERUP (top). (Bonus
    // pickup flash needs the QuakeC "bf" stuffcmd, not yet wired.)
    let eye_contents = quake_rs::world::point_contents(&w.bsp, eye);
    // Underwater sine wobble (D_WarpScreen): when the eye is in water/slime/lava
    // (contents <= CONTENTS_WATER, r_waterwarp default on), warp the 3-D frame
    // BEFORE the content tint so the screen ripples, not just darkens.
    if eye_contents <= quake_rs::bsp::CONTENTS_WATER {
        render::apply_warp(&mut view, w.clock); // D_WarpScreen warps the vrect only
    }
    // The screen: the view at its rectangle, backtile around it
    // (SCR_UpdateScreen's Draw_TileClear), the status bar drawn over below.
    let backtile = backtile_for(&vrect, render_w, render_h, w.gfx_wad.as_ref());
    let mut img =
        render::compose_view(view, vrect, render_w, render_h, backtile.as_ref(), &w.palette);
    let mut shifts: Vec<([u8; 3], f32)> = Vec::new();
    if let Some(cs) = render::content_cshift(eye_contents) {
        shifts.push(cs);
    }
    if w.damage_blend > 0.0 {
        shifts.push((w.damage_color, w.damage_blend));
    }
    // Powerup tint (Quad=blue, Biosuit=green, Ring=gray, Pentagram=yellow).
    if let Some(cs) = render::powerup_cshift(w.server.vm.ent_get_float(w.player, "items") as i32) {
        shifts.push(cs);
    }
    // V_UpdatePalette (software view.c): the cshift is a whole-PALETTE shift run
    // LAST in SCR_UpdateScreen, so it tints the ENTIRE screen — 3D view, status bar,
    // centerprint, menu, console — not just the 3D viewport (that 3D-only scope is
    // the GLQuake R_PolyBlend look). We DEFER the blend: draw the HUD/messages on the
    // untinted frame and return (color, alpha) so the dispatcher tints the fully
    // composited frame (after the menu/console overlay too).
    let blend = if shifts.is_empty() {
        ([0u8, 0, 0], 0.0f32)
    } else {
        render::combine_cshifts(&shifts)
    };
    bench::lap(Phase::Post3d);

    // 6. Status bar (HUD) overlay: blit the bottom bar with the player's live
    //    health/ammo/armour on top of the finished 3-D frame. Skipped silently
    //    when gfx.wad was absent (the world still renders).
    //
    //    During an intermission SCR_UpdateScreen (screen.c) draws the matching
    //    overlay INSTEAD of the status bar — Sbar_IntermissionOverlay for
    //    cl.intermission == 1, Sbar_FinaleOverlay + the revealed center string
    //    for == 2, the center string alone for == 3 — and only while the game
    //    owns the screen (`key_dest == key_game`; with the menu/console up
    //    neither the bar nor the overlay paints, the view is full-screen).
    if w.intermission != 0 {
        if !menu_up {
            match w.intermission {
                1 => {
                    if let Some(wad) = w.gfx_wad.as_ref() {
                        // Counts from the QuakeC globals the engine's
                        // SV_UpdateStats reads (same source as the Tab scoreboard).
                        let gcount = |g: &str| w.server.vm.gget_float(g) as i32;
                        let stats = render::IntermissionStats {
                            // cl.completed_time is an int in the C: whole seconds.
                            completed_time: w.completed_time as i32,
                            secrets: gcount("found_secrets"),
                            total_secrets: gcount("total_secrets"),
                            monsters: gcount("killed_monsters"),
                            total_monsters: gcount("total_monsters"),
                        };
                        render::draw_intermission_overlay(
                            &mut img,
                            wad,
                            &w.palette,
                            w.pic_complete.as_ref(),
                            w.pic_inter.as_ref(),
                            &stats,
                        );
                    }
                }
                2 => render::draw_finale_overlay(
                    &mut img,
                    w.conchars.as_ref(),
                    &w.palette,
                    w.pic_finale.as_ref(),
                    &w.finale_text,
                    w.clock - w.finale_start,
                ),
                // svc_cutscene: the centered text alone, no plaque.
                _ => render::draw_finale_overlay(
                    &mut img,
                    w.conchars.as_ref(),
                    &w.palette,
                    None,
                    &w.finale_text,
                    w.clock - w.finale_start,
                ),
            }
        }
    } else if let Some(wad) = w.gfx_wad.as_ref() {
        let stat = |f: &str| w.server.vm.ent_get_float(w.player, f) as i32;
        // Solo-scoreboard counts come from the QuakeC globals the engine's
        // SV_UpdateStats reads; the level name is worldspawn's `message` (edict 0).
        let gcount = |g: &str| w.server.vm.gget_float(g) as i32;
        let level_name = w.server.vm.ent_get_string(0, "message");
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
            // Sbar_SoloScoreboard shows cl.time — the SERVER clock (epoch 1.0,
            // SV_SpawnServer), not this walk's 0-based clock, matching what the
            // intermission overlay's completed_time latches.
            time: w.server.time(),
            monsters: gcount("killed_monsters"),
            total_monsters: gcount("total_monsters"),
            secrets: gcount("found_secrets"),
            total_secrets: gcount("total_secrets"),
            level_name: &level_name,
            // Tab "show scores" isn't wired as a key yet; the dead-player branch
            // (health <= 0) inside draw_hud_into handles the death scoreboard.
            show_scores: false,
            sb_lines: refdef.sb_lines,
        };
        render::draw_hud_into(&mut img, &hud);
    }

    // On-screen messages the QuakeC printed (drained above): the current
    // centerprint drawn centered, the notify lines stacked top-left. Both time
    // out via their stored expiry; drawn over the HUD. Suppressed while the menu or
    // console owns the screen (Quake draws the notify/centerprint only for
    // key_dest == key_game), so they don't paint through the menu/console overlay.
    // Also suppressed during intermission: SCR_UpdateScreen's intermission
    // branches draw neither SCR_CheckDrawCenterString (the finale text above is
    // its own path) nor the console notify lines.
    if !menu_up && w.intermission == 0 {
        if let Some(cc) = w.conchars.as_ref() {
            if let Some((text, _)) = &w.centerprint {
                render::draw_centerprint(&mut img, cc, &w.palette, text);
            }
            if !w.notify.is_empty() {
                let lines: Vec<&str> = w.notify.iter().map(|(t, _)| t.as_str()).collect();
                render::draw_notify(&mut img, cc, &w.palette, &lines);
            }
        }
    }

    // The main-menu overlay is drawn by the `step` dispatcher (the menu lives at
    // the App level now so it can overlay walk OR the attract demo); step_walk no
    // longer draws it. The deferred screen blend rides out with the frame so the
    // dispatcher tints the whole composited image (HUD + menu + console included).
    bench::lap(Phase::Hud2d);
    (img, blend.0, blend.1)
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
    let sounds = frame.sounds.clone();
    let stops = frame.stop_sounds.clone();
    let damage = frame.damage.clone();
    let prints = frame.prints.clone();
    let centerprints = frame.centerprints.clone();
    let view_entity_origin = frame.view_entity_origin;
    let view_angles = frame.view_angles;
    for b in &bursts {
        // svc_particle is always R_RunParticleEffect (spawn_burst) in id's
        // CL_ParseParticleEffect — the net count==255 sentinel just means 1024
        // particles (the demo parser already maps it), NOT the rocket
        // R_ParticleExplosion. Route every burst through spawn_burst.
        d.particles
            .spawn_burst(b.org, b.dir, b.color, b.count, now, &mut d.prng);
    }
    // CLIENT-SIDE temp-entity impact sounds (CL_ParseTEnt: tink/ric for
    // spikes, wizard/hit, hknight/hit, r_exp3 for explosions) — the C plays
    // these during demo playback too; they are NOT in the recorded svc_sound
    // stream. Collected here and queued through the same path as the recorded
    // sounds below.
    let mut te_sounds: Vec<quake_rs::server::SoundEvent> = Vec::new();
    for ev in &tents {
        // Beam types refresh the entity's beam slot (CL_ParseBeam) with the
        // frame's recorded server time; step_demo expands the live beams into
        // bolt-model instances every render (CL_UpdateTEnts), exactly like the
        // live walk.
        if let Some(bm) = BeamModel::from_te_type(ev.te_type) {
            d.beams.parse_beam(ev.entity, bm, ev.pos, ev.end, now);
            continue;
        }
        // Reuse the live-walk mapping (explosion/impact/splash) — including
        // its client-side impact sound, exactly like step_walk's te_sounds.
        if let Some(name) = spawn_temp_entity(&mut d.particles, ev, now, &mut d.prng) {
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
    // The RECORDED svc_sound one-shots (CL_ParseStartSoundPacket ->
    // S_StartSound): queue through the SAME spatialized path live play uses.
    // The listener is the recorded camera pose, which step_demo refreshes
    // every frame; the recorded view entity's own sounds (weapon fire, pain
    // grunts) get the full-volume centred treatment via the view_entity key.
    if !sounds.is_empty() {
        queue_sounds(&d.pak, &sounds, d.demo.viewentity as i32);
    }
    if !te_sounds.is_empty() {
        queue_sounds(&d.pak, &te_sounds, d.demo.viewentity as i32);
    }
    // svc_stopsound: hand the (entity, channel) stops to the page, which
    // stop()s its registered source for that key (S_StopSound).
    push_stop_sounds(&stops);
    // svc_damage (V_ParseDamage, view.c): bump the damage cshift and compute
    // the directional view kick from the recorded attack origin.
    for dmg in &damage {
        // count = blood*0.5 + armor*0.5, floored at 10; percent += 3*count,
        // clamped 0..150.
        let count = (dmg.blood as f32 * 0.5 + dmg.armor as f32 * 0.5).max(10.0);
        d.damage_blend = (d.damage_blend + 3.0 * count).clamp(0.0, 150.0);
        d.damage_color = if dmg.armor > dmg.blood {
            [200, 100, 100] // armour absorbed most -> pinkish
        } else if dmg.armor > 0 {
            [220, 50, 50] // some armour -> orange-red
        } else {
            [255, 0, 0] // pure blood -> red
        };
        // from = normalize(from - ent->origin); AngleVectors(ent->angles) with
        // the angles V_CalcRefdef maintains on the view entity: YAW =
        // cl.viewangles[YAW], PITCH = -cl.viewangles[PITCH], ROLL untouched
        // (~0 for the player).
        let delta = [
            dmg.from[0] - view_entity_origin[0],
            dmg.from[1] - view_entity_origin[1],
            dmg.from[2] - view_entity_origin[2],
        ];
        let (from_dir, _len) = quake_rs::math::normalize(delta);
        let (forward, right, _up) =
            quake_rs::math::angle_vectors([-view_angles[0], view_angles[1], 0.0]);
        // v_kickroll 0.6 / v_kickpitch 0.6 / v_kicktime 0.5 (stock cvars).
        d.v_dmg_roll = count * quake_rs::math::dot(from_dir, right) * V_KICKROLL;
        d.v_dmg_pitch = count * quake_rs::math::dot(from_dir, forward) * V_KICKPITCH;
        d.v_dmg_time = V_KICKTIME;
    }
    // svc_print fragments accumulate Con_Print-style (a notify line breaks
    // only on '\n' — pickups arrive as several fragments) with Quake's
    // con_notifytime expiry on the demo's recorded clock; svc_centerprint
    // replaces the current centered message (SCR_CenterPrint, ~2 s).
    for p in &prints {
        d.notify_pending.push_str(p);
    }
    while let Some(nl) = d.notify_pending.find('\n') {
        let line: String = d.notify_pending.drain(..=nl).collect();
        let line = line.trim_end_matches(['\n', '\r']).to_string();
        if !line.trim().is_empty() {
            d.notify.push((line, now + 3.0));
            while d.notify.len() > 4 {
                d.notify.remove(0);
            }
        }
    }
    if let Some(text) = centerprints.into_iter().next_back() {
        d.centerprint = Some((text, now + 2.0));
    }
}

/// `v_kicktime` (view.c, default "0.5"): how long an svc_damage view kick lasts.
const V_KICKTIME: f32 = 0.5;
/// `v_kickroll` (view.c, default "0.6"): roll degrees per damage count*side.
const V_KICKROLL: f32 = 0.6;
/// `v_kickpitch` (view.c, default "0.6"): pitch degrees per damage count*side.
const V_KICKPITCH: f32 = 0.6;

fn step_demo(
    d: &mut DemoPlay,
    dt: f32,
    menu_up: bool,
    render_w: usize,
    render_h: usize,
) -> (render::Image, [u8; 3], f32) {
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
        // and beam and forget what was spawned so the replay from frame 0 is
        // identical to the first pass (no stale explosions/bolts carried across
        // the wrap). The per-POV view state resets too: damage flash/kick,
        // notify + centerprint text, and the stair-smoothing accumulator (their
        // expiries live on the recorded clock, which just jumped back to t0).
        d.particles = ParticleSystem::new();
        d.beams.clear();
        d.last_spawned_idx = usize::MAX;
        d.damage_blend = 0.0;
        d.v_dmg_time = 0.0;
        d.centerprint = None;
        d.notify.clear();
        d.notify_pending.clear();
        d.oldz = f32::NAN;
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
    let mut bmodels: Vec<render::BModelInstance> = Vec::new();
    let mut sprite_insts: Vec<render::SpriteInstance> = Vec::new();
    for e in &f.entities {
        if let Some(Some(mdl)) = d.models.get(e.modelindex) {
            owned.push(ModelInstance {
                mdl,
                origin: e.origin,
                yaw: e.angles[1],
                // Demo entities carry full angles; orient projectiles (pitch/roll)
                // as the recorded stream did (R_AliasSetUpTransform).
                pitch: e.angles[0],
                roll: e.angles[2],
                color: d.colors.get(e.modelindex).copied().unwrap_or([200, 200, 200]),
                // Demo entities carry their current animation frame from the net
                // stream — use it so monsters in the demo are actually posed.
                frame: e.frame.max(0) as usize,
                skinnum: 0,
            });
        } else if let Some(num) = d
            .demo
            .model_precache
            .get(e.modelindex)
            .and_then(|name| name.strip_prefix('*'))
            .and_then(|n| n.parse::<usize>().ok())
        {
            // Brush submodels (doors, platforms, ELEVATORS, buttons) are "*N"
            // precache names with no alias Mdl; they render at the entity origin
            // from world submodel N. The live walk passes these; the demo path used
            // to drop them entirely, so moving level geometry vanished behind the
            // boot-demo menu. `frame` picks the activated (+a..+j) texture cycle.
            bmodels.push(render::BModelInstance {
                model_index: num,
                origin: e.origin,
                frame: e.frame.max(0),
            });
        } else if let Some(Some(spr)) = d.sprites.get(e.modelindex) {
            // Sprite-model entity (the boot demo's s_explod.spr explosion flashes).
            sprite_insts.push(render::SpriteInstance {
                sprite: spr,
                origin: e.origin,
                frame: e.frame.max(0) as usize,
            });
        }
    }
    // CL_UpdateTEnts: expand the recorded lightning beams into bolt-model
    // pieces, exactly like the live walk. The bolt models resolve through the
    // demo's PRECACHE table (the .dem signon lists progs/bolt*.mdl); a beam
    // owned by the recorded view entity tracks its per-frame origin.
    if d.beams.any_live(f.time) {
        d.beams.update(
            f.time,
            d.demo.viewentity as i32,
            f.view_entity_origin,
            &mut d.prng,
            &mut d.beam_scratch,
        );
        for seg in &d.beam_scratch {
            let name = seg.model.model_name();
            let Some(idx) = d.demo.model_precache.iter().position(|n| n == name) else {
                continue; // model not precached (e.g. beam.mdl in shareware)
            };
            if let Some(Some(mdl)) = d.models.get(idx) {
                owned.push(ModelInstance {
                    mdl,
                    origin: seg.origin,
                    yaw: seg.yaw,
                    pitch: seg.pitch,
                    roll: seg.roll,
                    color: d.colors.get(idx).copied().unwrap_or([200, 200, 200]),
                    frame: 0,
                    skinnum: 0,
                });
            }
        }
    }
    // The recorded per-client state (svc_clientdata) drives V_CalcRefdef.
    let client = f.client;
    let cam = if f.intermission != 0 {
        // V_CalcIntermissionRefdef (view.c): a recorded intermission renders
        // with the forced v_idlescale=1 idle sway (V_AddIdle, stock
        // cycle/level cvars) applied LIVE on top of the recorded (QC-placed)
        // camera angles — no bob, no punch, no kick, no stair smoothing.
        Camera {
            pos: f.view_origin,
            yaw: f.view_angles[1] + (f.time * 2.0).sin() * 0.3,
            pitch: -(f.view_angles[0] + (f.time * 1.0).sin() * 0.3),
            roll: f.view_angles[2] + (f.time * 0.5).sin() * 0.1,
            fov_deg: 90.0,
        }
    } else {
        // V_CalcRefdef (view.c) on the RECORDED stream, exactly like the C's
        // demo playback: head-bob from the recorded SU_VELOCITY (visible in
        // id's demo1 — the player runs), stair-step smoothing from the
        // recorded SU_ONGROUND, the strafe/damage/dead view roll
        // (V_CalcViewRoll) and the recorded punchangle added LAST. V_AddIdle
        // is a no-op here (v_idlescale defaults to 0 outside intermission);
        // the 1/32 anti-node-line epsilon is omitted, matching this port's
        // live walk. The listener pose below deliberately stays UNbobbed
        // (audio panning must not jitter with the head-bob), like step_walk.
        let vel = client.velocity;
        let speed_xy = (vel[0] * vel[0] + vel[1] * vel[1]).sqrt();
        let bob = render::view_bob(speed_xy, f.time);
        let mut eye = f.view_origin; // view entity origin + recorded viewheight
        eye[2] += bob;
        // Stair-step smoothing (V_CalcRefdef ~960): the same port as
        // step_walk's, driven by the recorded onground flag + the raw view
        // entity origin z.
        let origin_z = f.view_entity_origin[2];
        let sdt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        if d.oldz.is_finite() && client.onground && origin_z - d.oldz > 0.0 {
            d.oldz += sdt * 80.0;
            if d.oldz > origin_z {
                d.oldz = origin_z;
            }
            if origin_z - d.oldz > 12.0 {
                d.oldz = origin_z - 12.0;
            }
            eye[2] += d.oldz - origin_z;
        } else {
            d.oldz = origin_z;
        }
        // V_CalcViewRoll: strafe lean from the recorded velocity (the C reads
        // the view entity's angles, which V_CalcRefdef keeps at YAW =
        // viewangles[YAW], PITCH = -viewangles[PITCH]), plus the decaying
        // svc_damage kick; a dead POV (health <= 0) REPLACES the whole roll
        // with the 80-degree dead view (the recorded viewangles[ROLL] and the
        // lean/kick are wiped — the C assigns viewangles[ROLL] = 80). The
        // punchangle adds AFTER, per the C's VectorAdd ordering.
        let basis = [-f.view_angles[0], f.view_angles[1], 0.0];
        let mut roll_angle =
            f.view_angles[2] + quake_rs::server::v_calc_roll(basis, vel);
        let mut dmg_pitch = 0.0;
        if d.v_dmg_time > 0.0 {
            roll_angle += d.v_dmg_time / V_KICKTIME * d.v_dmg_roll;
            dmg_pitch = d.v_dmg_time / V_KICKTIME * d.v_dmg_pitch;
            d.v_dmg_time -= sdt; // v_dmg_time -= host_frametime
        }
        if client.health <= 0 {
            roll_angle = 80.0; // dead view angle (replaces lean + kick + bank)
        }
        Camera {
            pos: eye,
            yaw: f.view_angles[1] + client.punchangle[1],
            // QuakeC pitch is +down; the renderer's is +up.
            pitch: -(f.view_angles[0] + dmg_pitch + client.punchangle[0]),
            roll: roll_angle + client.punchangle[2],
            fov_deg: 90.0,
        }
    };
    // Sound listener pose + the per-leaf ambient channels follow the demo
    // camera (the C's S_Update runs in demo playback too — the recorded e1m3
    // run drifts past water and open sky, and its placed torch loops pan with
    // the recorded view). Forward/right are the level yaw basis like step_walk.
    {
        let yaw_rad = (f.view_angles[1] as f64).to_radians();
        let (sy, cy) = (yaw_rad.sin() as f32, yaw_rad.cos() as f32);
        LISTENER.with(|l| {
            *l.borrow_mut() = Listener {
                pos: f.view_origin,
                forward: [cy, sy, 0.0],
                right: [sy, -cy, 0.0],
            };
        });
        update_ambient_channels(&d.bsp, f.view_origin, dt);
    }
    // The recorded server time animates the demo's liquids/sky too. The live
    // particle pool (replayed from the recorded svc_particle / temp-entity
    // stream) is passed as (world pos, palette index) so blood/puffs/explosions
    // draw into the scene sharing its z-buffer. Demos carry no dynamic lights
    // here (empty; a deferred LOW).
    let parts: Vec<([f32; 3], u8)> =
        d.particles.particles().iter().map(|p| (p.origin, p.color)).collect();
    // The RECORDED svc_lightstyle table drives the world lighting through the
    // same R_AnimateLight 10 Hz logic the live walk uses (lightstyle_scales_at)
    // — the demo's torch flicker matches the recording exactly. A synthetic
    // demo without a table (tests) falls back to the previous seeded default:
    // style 0 = 'm' (264/256, id's steady-world brightness), the rest neutral.
    let demo_styles = if f.lightstyles.is_empty() {
        let mut s = render::NEUTRAL_LIGHTSTYLE_SCALES;
        s[0] = 264.0 / 256.0;
        s
    } else {
        quake_rs::server::lightstyle_scales_at(&f.lightstyles, f.time)
    };
    // The first-person weapon viewmodel: SU_WEAPON is the model PRECACHE index
    // (`view->model = cl.model_precache[cl.stats[STAT_WEAPON]]`, V_CalcRefdef),
    // SU_WEAPONFRAME its animation frame. Hidden exactly like R_DrawViewModel
    // (r_main.c ~606): invisible POV (Ring of Shadows), dead POV, or an
    // intermission (V_CalcIntermissionRefdef sets `view->model = NULL`).
    let hide_gun = f.intermission != 0
        || client.health <= 0
        || client.items & IT_INVISIBILITY != 0;
    let viewmodel = if hide_gun {
        None
    } else {
        match d.models.get(client.weapon_model.max(0) as usize) {
            Some(Some(mdl)) => {
                // V_CalcRefdef's gun origin from the recorded velocity's bob.
                let vel = client.velocity;
                let bob = render::view_bob((vel[0] * vel[0] + vel[1] * vel[1]).sqrt(), f.time);
                // CalcGunAngle: the recorded view (with the damage kick's
                // pitch) before the punch, and the recorded roll.
                let angles = render::viewmodel_angles(&cam, client.punchangle, f.view_angles[2]);
                Some(Viewmodel {
                    mdl,
                    frame: client.weaponframe.max(0) as usize,
                    origin_ofs: render::viewmodel_origin_ofs(angles, bob, d.viewsize),
                    angles,
                })
            }
            _ => None,
        }
    };
    // SCR_CalcRefdef: the same viewsize framing as live play (the C's demo IS
    // the client rendering a recorded stream).
    bench::lap(Phase::Sim);
    let refdef = render::calc_refdef(render_w, render_h, d.viewsize, f.intermission != 0);
    let vrect = refdef.vrect;
    let mut view = render::render_scene_ext_sprited(&d.bsp, &cam, vrect.w, vrect.h, &d.palette, &owned, &bmodels, &[], viewmodel, f.time, &parts, &[], &demo_styles, d.colormap.as_deref(), &sprite_insts);
    bench::lap(Phase::Render3d);
    // D_WarpScreen: a submerged recorded POV ripples exactly like live play —
    // the warp applies to the 3-D view FIRST; the content tint joins the
    // deferred whole-screen blend below (V_CalcBlend order).
    let eye_contents = quake_rs::world::point_contents(&d.bsp, cam.pos);
    if eye_contents <= quake_rs::bsp::CONTENTS_WATER {
        render::apply_warp(&mut view, f.time);
    }
    let backtile = backtile_for(&vrect, render_w, render_h, d.gfx_wad.as_ref());
    let mut img =
        render::compose_view(view, vrect, render_w, render_h, backtile.as_ref(), &d.palette);
    bench::lap(Phase::Post3d);
    // A recorded intermission/finale frame draws its overlay exactly like the
    // live walk (SCR_UpdateScreen's cl.intermission branches), gated on the game
    // owning the screen (`key_dest == key_game` — i.e. no menu/console up).
    if f.intermission != 0 && !menu_up {
        match f.intermission {
            1 => {
                if let Some(wad) = d.gfx_wad.as_ref() {
                    let stats = render::IntermissionStats {
                        completed_time: f.completed_time as i32,
                        secrets: f.stats.secrets,
                        total_secrets: f.stats.total_secrets,
                        monsters: f.stats.monsters,
                        total_monsters: f.stats.total_monsters,
                    };
                    render::draw_intermission_overlay(
                        &mut img,
                        wad,
                        &d.palette,
                        d.pic_complete.as_ref(),
                        d.pic_inter.as_ref(),
                        &stats,
                    );
                }
            }
            2 => render::draw_finale_overlay(
                &mut img,
                d.conchars.as_ref(),
                &d.palette,
                d.pic_finale.as_ref(),
                &f.finale_text,
                f.time - f.finale_start,
            ),
            _ => render::draw_finale_overlay(
                &mut img,
                d.conchars.as_ref(),
                &d.palette,
                None,
                &f.finale_text,
                f.time - f.finale_start,
            ),
        }
    } else if let Some(wad) = d.gfx_wad.as_ref() {
        // Status bar from the RECORDED cl.stats (svc_clientdata) — the C's
        // Sbar_Draw runs identically during demo playback, so the attract loop
        // shows the recorded player's health/ammo/armour/items exactly like
        // live play. Drawn under the menu/console like step_walk's HUD (the C
        // draws the sbar regardless of key_dest; overlays paint on top).
        // draw_hud_into's dead-player branch shows the solo scoreboard when
        // the recorded health hits 0, like Sbar_Draw's scoreboard flip.
        let hud = render::Hud {
            wad,
            palette: &d.palette,
            health: client.health,
            ammo: client.ammo,
            armor: client.armor,
            items: client.items,
            weapon: client.active_weapon,
            ammo_shells: client.shells,
            ammo_nails: client.nails,
            ammo_rockets: client.rockets,
            ammo_cells: client.cells,
            // The recorded server clock (cl.time) drives the weapon-flash
            // cycle + face animation, exactly what sbar.c reads.
            time: f.time,
            monsters: f.stats.monsters,
            total_monsters: f.stats.total_monsters,
            secrets: f.stats.secrets,
            total_secrets: f.stats.total_secrets,
            level_name: &d.demo.level_name,
            show_scores: false,
            sb_lines: refdef.sb_lines,
        };
        render::draw_hud_into(&mut img, &hud);
    }

    // On-screen messages from the recorded svc_print / svc_centerprint stream,
    // drawn through the same overlays live play uses, with the same key_dest +
    // intermission gating as step_walk. Expiries live on the recorded clock.
    if let Some((_, exp)) = &d.centerprint {
        if f.time >= *exp {
            d.centerprint = None;
        }
    }
    let ftime = f.time;
    d.notify.retain(|(_, exp)| ftime < *exp);
    if !menu_up && f.intermission == 0 {
        if let Some(cc) = d.conchars.as_ref() {
            if let Some((text, _)) = &d.centerprint {
                render::draw_centerprint(&mut img, cc, &d.palette, text);
            }
            if !d.notify.is_empty() {
                let lines: Vec<&str> = d.notify.iter().map(|(t, _)| t.as_str()).collect();
                render::draw_notify(&mut img, cc, &d.palette, &lines);
            }
        }
    }

    // Screen blends (V_CalcBlend order: CONTENTS -> DAMAGE -> POWERUP), all
    // from the RECORDED stream: the eye-contents tint, the svc_damage flash
    // (faded dt*150 per frame like V_UpdatePalette), and the powerup tint from
    // the recorded cl.items. DEFERRED to the dispatcher so it tints the whole
    // composited frame (HUD + menu + console), like the live walk.
    {
        let sdt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        d.damage_blend = (d.damage_blend - sdt * 150.0).max(0.0);
    }
    let mut shifts: Vec<([u8; 3], f32)> = Vec::new();
    if let Some(cs) = render::content_cshift(eye_contents) {
        shifts.push(cs);
    }
    if d.damage_blend > 0.0 {
        shifts.push((d.damage_color, d.damage_blend));
    }
    if let Some(cs) = render::powerup_cshift(client.items) {
        shifts.push(cs);
    }
    let blend = if shifts.is_empty() {
        ([0u8, 0, 0], 0.0f32)
    } else {
        render::combine_cshifts(&shifts)
    };
    bench::lap(Phase::Hud2d);
    (img, blend.0, blend.1)
}

#[cfg(test)]
#[path = "census_tests.rs"]
mod census_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::{console_toggle, console_visible};
    use crate::input::*;
    use crate::menu::*;
    use crate::snd_dma::*;
    use crate::vid::*;
    use crate::test_util::*;
    use quake_rs::server::{SoundEvent, TempEntityEvent};

    #[test]
    fn step_demo_shows_the_last_frame_before_looping() {
        // FIX-7: the wrap must be DEFERRED so frames[n-1] is rendered for one
        // step before looping back to frame 0. The old code reset to 0 the
        // instant `idx` reached n-1, so the final frame was never displayed.
        use quake_rs::demo::{Demo, DemoFrame};

        let frame = |t: f32| DemoFrame { time: t, ..Default::default() };
        let demo = Demo {
            level_name: "test".into(),
            static_sounds: Vec::new(),
            // map_name() reads model_precache[1]; unused by step_demo's indexing.
            model_precache: vec![String::new(), "maps/test.bsp".into()],
            sound_precache: Vec::new(),
            viewentity: 0,
            // Three frames at t = 0, 1, 2.
            frames: vec![frame(0.0), frame(1.0), frame(2.0)],
        };
        let mut d = DemoPlay {
            bsp: render::demo_room(),
            palette: [[0u8; 3]; 256],
            demo,
            models: Vec::new(),
            sprites: Vec::new(),
            colormap: None,
            colors: Vec::new(),
            elapsed: 0.0,
            idx: 0,
            particles: ParticleSystem::new(),
            prng: Lcg::new(1),
            last_spawned_idx: usize::MAX,
            beams: Beams::new(),
            beam_scratch: Vec::new(),
            gfx_wad: None,
            conchars: None,
            pic_complete: None,
            pic_inter: None,
            pic_finale: None,
            pak: build_test_pak(&[]),
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
        };
        let n = d.demo.frames.len();

        // Drive several 1.0s steps and record which frame index is RENDERED
        // (i.e. the value of `idx` chosen by step_demo for that frame).
        let mut shown = Vec::new();
        for _ in 0..5 {
            let _img = step_demo(&mut d, 1.0, false, DEFAULT_W, DEFAULT_H);
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

        let plain = |t: f32| DemoFrame { time: t, ..Default::default() };
        // Frame 1 (t=0.05) carries the effects; frames 0 and 2 are empty. Frame
        // times are one ~Quake tick apart so a 0.05s step advances exactly one
        // frame and the explosion's ramp ages by a realistic amount (not all the
        // way through its 8-frame life in a single huge step).
        let effect_frame = DemoFrame {
            time: 0.05,
            view_origin: [0.0, 0.0, 0.0],
            view_entity_origin: [0.0, 0.0, 0.0],
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
                end: [10.0, 0.0, 0.0],
                entity: 0,
                color_start: 0,
                color_length: 0,
            }],
            ..Default::default()
        };
        let demo = Demo {
            level_name: "test".into(),
            static_sounds: Vec::new(),
            model_precache: vec![String::new(), "maps/test.bsp".into()],
            sound_precache: Vec::new(),
            viewentity: 0,
            frames: vec![plain(0.0), effect_frame, plain(0.10)],
        };
        let mut d = DemoPlay {
            bsp: render::demo_room(),
            palette: [[0u8; 3]; 256],
            demo,
            models: Vec::new(),
            sprites: Vec::new(),
            colormap: None,
            colors: Vec::new(),
            elapsed: 0.0,
            idx: 0,
            particles: ParticleSystem::new(),
            prng: Lcg::new(1),
            last_spawned_idx: usize::MAX,
            beams: Beams::new(),
            beam_scratch: Vec::new(),
            gfx_wad: None,
            conchars: None,
            pic_complete: None,
            pic_inter: None,
            pic_finale: None,
            pak: build_test_pak(&[]),
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
        };

        // Step 0.05s: lands on frame 1 (the effect frame). The burst (20) +
        // explosion (1024) particles populate the pool; after one tick of aging
        // the bulk of the 1024-particle explosion is still alive.
        let _ = step_demo(&mut d, 0.05, false, DEFAULT_W, DEFAULT_H);
        assert_eq!(d.idx, 1, "advanced onto the effect frame");
        let after_first = d.particles.len();
        assert!(
            after_first > 500,
            "the burst + 1024-particle explosion populate the pool (got {after_first})"
        );

        // A tiny step that holds us on frame 1 must NOT re-spawn the explosion
        // (the pool only shrinks as particles age — it never jumps back up).
        let _ = step_demo(&mut d, 0.001, false, DEFAULT_W, DEFAULT_H);
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
            let _ = step_demo(&mut d, 0.05, false, DEFAULT_W, DEFAULT_H);
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

    /// Regression for the one-time texture/lighting "pops" in the first second of
    /// live play (two distinct root causes, both whole-view shimmers):
    ///
    /// 1. **Spawn settle on screen.** QuakeC `PutClientInServer` places the player
    ///    at `spot.origin + '0 0 1'` (the start map's spawn floats ~5 units up),
    ///    and the port used to render frame 0 with ZERO physics frames after the
    ///    spawn — the player fell to the floor ON SCREEN over the first 2-3 frames
    ///    and every textured surface resampled (17.8-45.9% of pixels/frame).
    ///    WinQuake runs two SV_Physics ticks during the signon (the Host_Spawn_f
    ///    and Host_Begin_f frames) before SCR_EndLoadingPlaque re-enables drawing;
    ///    `Server::run_signon_frames` ports those, and every walk-building path
    ///    (boot / New Game / changelevel / restart) must call it.
    /// 2. **Plane-only dlight gating.** ~0.55s in (sv.time ~1.98 with the current
    ///    deterministic PRNG), the start map's distant `misc_fireball` lavaball
    ///    spawns with a rocket-trail dynamic light ~2000 units away behind walls;
    ///    `any_dlight_reaches`' old plane-distance-only test marked every
    ///    near-coplanar face in the VIEW as dynamically lit, kicking them off the
    ///    baked surface cache onto the per-pixel path (13.3% of pixels shifted in
    ///    one frame, then back when the light died). The gate now also tests the
    ///    face's texture-space extent (WinQuake's R_MarkLights is spatially
    ///    bounded by the BSP recursion).
    ///
    /// The 45-frame window covers both: settle would hit frames 0-2, the fireball
    /// ~frame 34.
    #[test]
    fn new_game_first_frames_render_a_settled_player_no_pop() {
        // The browser path: attract demo + menu, then Single Player > New Game.
        assert_eq!(boot_attract(), 1);
        step(1.0 / 60.0); // an attract-demo frame, like the live page
        menu_select(); // Main: Single Player
        menu_select(); // SP: New Game -> builds the start-map walk, closes menu

        // BEFORE the first frame renders the player must already be settled:
        // on the ground, no residual fall velocity (it spawns ~5 units up).
        let eye0 = APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            let w = a.walk.as_ref().unwrap();
            let flags = w.server.vm.ent_get_float(w.player, "flags") as i32;
            let vel = w.server.vm.ent_get_vector(w.player, "velocity");
            assert!(flags & FL_ONGROUND != 0, "player on the ground at frame 0");
            assert_eq!(vel[2], 0.0, "no residual fall velocity at frame 0");
            w.server.player_view().0
        });

        // Frame 0, then 44 more static zero-input frames: the eye must stay
        // bit-identical (the settle pop was exactly this eye motion leaking into
        // the first rendered frames)...
        step(1.0 / 60.0);
        let mut prev: Vec<u8> = APP.with(|c| c.borrow().as_ref().unwrap().fb.clone());
        let (w, h) = APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            (a.render_w, a.render_h)
        });
        for i in 1..45 {
            step(1.0 / 60.0);
            let eye = APP.with(|c| {
                let b = c.borrow();
                let a = b.as_ref().unwrap();
                a.walk.as_ref().unwrap().server.player_view().0
            });
            assert_eq!(eye, eye0, "static eye is bit-identical on frame {i}");
            // ...and consecutive frames stay near-identical. Faithful animation
            // in the static spawn view (scrolling sky, flame group-frames, the
            // 10 Hz lightstyle flicker) touches <= ~0.4% of pixels per frame;
            // the settle pop touched 17.8%-45.9% and the fireball-dlight pop
            // 13.3%. A 2% ceiling separates bug from animation with a wide
            // margin in both directions.
            let fb: Vec<u8> = APP.with(|c| c.borrow().as_ref().unwrap().fb.clone());
            let nd = prev
                .chunks_exact(4)
                .zip(fb.chunks_exact(4))
                .filter(|(a4, b4)| a4[..3] != b4[..3])
                .count();
            assert!(
                nd <= w * h / 50,
                "frame {i} vs {}: {nd} px differ ({:.2}%) — a one-time view shift leaked into the first frames",
                i - 1,
                100.0 * nd as f64 / (w * h) as f64
            );
            prev = fb;
        }
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
    fn thunderbolt_beam_renders_bolt_pixels_on_e1m1() {
        // End-to-end through the LIVE path: boot the e1m1 walk, cheat in the
        // thunderbolt (impulse 9 = all weapons + ammo, impulse 8 = lightning
        // gun), hold fire, and verify (a) the QuakeC's TE_LIGHTNING2 broadcast
        // landed in the beam store, (b) bolt2.mdl was loaded on demand at parse
        // time (Mod_ForName in CL_ParseTEnt), (c) the per-frame CL_UpdateTEnts
        // expansion produced pieces anchored at the muzzle (origin + '0 0 16',
        // W_FireLightning), and (d) the bolt actually changes rendered pixels —
        // an identical re-render (same rng, dt=0) with the beams cleared differs.
        let mut w = build_walk().expect("e1m1 walk boots from the embedded pak");
        // Let the spawn settle (telefrag effects, initial thinks).
        for _ in 0..10 {
            let _ = step_walk(&mut w, 0.05, false, 320, 200);
        }
        w.next_impulse = 9; // CheatCommand: all weapons + full cells
        let _ = step_walk(&mut w, 0.05, false, 320, 200);
        w.next_impulse = 8; // select the thunderbolt
        let _ = step_walk(&mut w, 0.05, false, 320, 200);
        // Hold fire across several frames (W_FireLightning re-broadcasts the
        // beam each weapon frame, exercising the same-entity slot REPLACEMENT).
        w.in_attack = true;
        for _ in 0..6 {
            let _ = step_walk(&mut w, 0.05, false, 320, 200);
        }
        assert!(
            w.beams.any_live(w.clock),
            "firing the thunderbolt put a live beam in the store"
        );
        assert!(
            matches!(w.model_cache.get("progs/bolt2.mdl"), Some(Some(_))),
            "TE_LIGHTNING2 loaded progs/bolt2.mdl on demand"
        );
        // The last frame's expansion is retained in the scratch buffer: the
        // thunderbolt is ONE beam (slot replacement, never stacked). The QuakeC
        // fires from origin + '0 0 16' with a 600-unit traceline, but
        // CL_UpdateTEnts re-anchors the VIEW entity's beam to the player's raw
        // ORIGIN every frame (the C quirk — the visible bolt hangs 16 units
        // below the muzzle), so the segment can run a hair over 600 units:
        // 1..=21 Bolt2 pieces.
        assert!(
            !w.beam_scratch.is_empty() && w.beam_scratch.len() <= 21,
            "one ~600-unit beam expands to 1..=21 pieces (got {})",
            w.beam_scratch.len()
        );
        assert!(
            w.beam_scratch.iter().all(|s| s.model == BeamModel::Bolt2),
            "thunderbolt pieces use bolt2.mdl"
        );
        // The first piece sits at the player ORIGIN: the WriteEntity short
        // carried the player edict number through the decoder (an int global —
        // a float read would have yielded ~0 and never matched w.player), and
        // the view-entity re-anchor replaced the broadcast start (origin+16).
        let player_origin = w.server.vm.ent_get_vector(w.player, "origin");
        let first = w.beam_scratch[0].origin;
        for i in 0..3 {
            assert!(
                (first[i] - player_origin[i]).abs() < 1.0,
                "first piece re-anchored to the player origin (axis {i}: {} vs {})",
                first[i],
                player_origin[i]
            );
        }

        // Pixel evidence: render the SAME state twice (dt = 0 -> no time passes,
        // restored rng -> identical dlight jitter draws), once with the live
        // beam and once with the store cleared. The ONLY difference is the bolt
        // model pieces, so differing pixels prove the bolt drew into the scene.
        let rng = w.prng;
        let (with_bolt, _, _) = step_walk(&mut w, 0.0, false, 320, 200);
        w.prng = rng;
        w.beams.clear();
        let (without_bolt, _, _) = step_walk(&mut w, 0.0, false, 320, 200);
        let diff = with_bolt
            .rgb
            .iter()
            .zip(without_bolt.rgb.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(
            diff > 0,
            "the rendered thunderbolt changes pixels vs the beam-less frame"
        );
        // Optional visual evidence: QUAKE_DUMP_BEAM=/some/dir dumps the two
        // frames as PPMs for eyeballing (never set in CI; the asserts above are
        // the real check).
        if let Ok(dir) = std::env::var("QUAKE_DUMP_BEAM") {
            for (img, name) in [(&with_bolt, "with-bolt"), (&without_bolt, "without-bolt")] {
                let mut buf = format!("P6\n{} {}\n255\n", img.w, img.h).into_bytes();
                for px in &img.rgb {
                    buf.extend_from_slice(px);
                }
                let _ = std::fs::write(format!("{dir}/beam-{name}.ppm"), buf);
            }
        }
    }

    #[test]
    fn demo_playback_replays_recorded_lightning_beams() {
        // The DEMO path: a synthetic frame carrying a recorded TE_LIGHTNING1
        // must refresh the beam store when playback advances onto it, expand
        // into bolt.mdl pieces on render, and clear on the loop wrap.
        use quake_rs::demo::{Demo, DemoFrame};
        use quake_rs::server::te_consts;

        let plain = |t: f32| DemoFrame {
            time: t,
            view_origin: [0.0, 0.0, 0.0],
            view_entity_origin: [0.0, 0.0, 0.0],
            view_angles: [0.0, 0.0, 0.0],
            entities: Vec::new(),
            particles: Vec::new(),
            temp_entities: Vec::new(),
            ..Default::default()
        };
        let mut bolt_frame = plain(0.05);
        bolt_frame.temp_entities = vec![TempEntityEvent {
            te_type: te_consts::TE_LIGHTNING1,
            pos: [0.0, 0.0, 0.0],
            end: [75.0, 0.0, 0.0],
            entity: 9,
            color_start: 0,
            color_length: 0,
        }];
        // The real bolt model from the embedded pak, at precache index 2 (the
        // .dem signon precaches progs/bolt.mdl; index 1 is the world).
        let bolt_mdl = pak()
            .and_then(|p| p.read_file("progs/bolt.mdl").ok().flatten())
            .and_then(|b| Mdl::parse(&b).ok())
            .expect("progs/bolt.mdl parses from the embedded pak");
        let demo = Demo {
            level_name: "test".into(),
            model_precache: vec![
                String::new(),
                "maps/test.bsp".into(),
                "progs/bolt.mdl".into(),
            ],
            sound_precache: Vec::new(),
            viewentity: 1,
            static_sounds: Vec::new(),
            frames: vec![plain(0.0), bolt_frame, plain(0.10)],
        };
        let mut d = DemoPlay {
            bsp: render::demo_room(),
            palette: [[0u8; 3]; 256],
            demo,
            models: vec![None, None, Some(bolt_mdl)],
            sprites: vec![None, None, None],
            colormap: None,
            colors: vec![[200; 3]; 3],
            elapsed: 0.0,
            idx: 0,
            particles: ParticleSystem::new(),
            prng: Lcg::new(1),
            last_spawned_idx: usize::MAX,
            beams: Beams::new(),
            beam_scratch: Vec::new(),
            gfx_wad: None,
            conchars: None,
            pic_complete: None,
            pic_inter: None,
            pic_finale: None,
            pak: build_test_pak(&[]),
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
        };

        // Advance onto the bolt frame: the recorded beam lands in the store and
        // the render expands it (75 units => 3 pieces at 0/30/60 along +x).
        let _ = step_demo(&mut d, 0.05, false, 160, 100);
        assert_eq!(d.idx, 1, "advanced onto the bolt frame");
        assert!(d.beams.any_live(0.05), "recorded TE_LIGHTNING1 refreshed a beam");
        assert_eq!(d.beam_scratch.len(), 3, "75 units expand to 3 pieces");
        assert!(d.beam_scratch.iter().all(|s| s.model == BeamModel::Bolt));

        // A lingering step does NOT re-parse (last_spawned_idx guard) but the
        // beam stays live until its 0.2 s endtime.
        let _ = step_demo(&mut d, 0.001, false, 160, 100);
        assert!(d.beams.any_live(0.05));

        // Advance to the LAST frame: at t=0.10 the beam (endtime 0.25) still
        // rides across frames — it is a client effect, not a per-frame one.
        let mut guard = 0;
        while d.idx != 2 {
            let _ = step_demo(&mut d, 0.05, false, 160, 100);
            guard += 1;
            assert!(guard < 10, "playback reaches the last frame");
        }
        assert!(
            d.beams.any_live(d.demo.frames[2].time),
            "beam still live on the last frame (t=0.10 < endtime 0.25)"
        );
        // The NEXT (tiny) step triggers the deferred loop wrap: back to frame 0
        // with the beam store cleared (no stale bolts carried into the replay;
        // the tiny dt keeps playback ON frame 0, before the bolt re-spawns).
        let _ = step_demo(&mut d, 0.001, false, 160, 100);
        assert_eq!(d.idx, 0, "playback wrapped");
        assert!(!d.beams.any_live(0.0), "the wrap cleared the beam store");
    }

    // -------------------------------------------------------------------
    // Intermission + finale (end-to-end against the real progs.dat)
    // -------------------------------------------------------------------

    /// The centre of the live map's `trigger_changelevel` brush volume (from the
    /// absmin/absmax its setmodel+link produced), to pin the player onto.
    fn changelevel_trigger_center() -> [f32; 3] {
        walk_mut(|w| {
            for e in 0..w.server.vm.num_edicts() {
                let ent = e as i32;
                if w.server.vm.edict_free.get(e).copied().unwrap_or(true) {
                    continue;
                }
                if w.server.vm.ent_get_string(ent, "classname") == "trigger_changelevel" {
                    let amin = w.server.vm.ent_get_vector(ent, "absmin");
                    let amax = w.server.vm.ent_get_vector(ent, "absmax");
                    return [
                        0.5 * (amin[0] + amax[0]),
                        0.5 * (amin[1] + amax[1]),
                        0.5 * (amin[2] + amax[2]),
                    ];
                }
            }
            panic!("no trigger_changelevel in the live map");
        })
    }

    /// Pin the player onto the exit trigger and step until the QuakeC's
    /// `execute_changelevel` think fires `svc_intermission` (touch at frame N,
    /// the scheduled think 0.1s later). Panics if it never arrives.
    fn drive_into_exit() {
        let centre = changelevel_trigger_center();
        for _ in 0..40 {
            walk_mut(|w| {
                let p = w.player;
                w.server.vm.ent_set_vector(p, "origin", centre);
                w.server.vm.ent_set_vector(p, "velocity", [0.0, 0.0, 0.0]);
            });
            step(0.1);
            if walk_mut(|w| w.intermission) != 0 {
                return;
            }
        }
        panic!("svc_intermission never arrived after 40 frames on the exit trigger");
    }

    /// Write the current RGBA framebuffer as a binary PPM into `$QUAKE_DUMP_DIR`
    /// (the feature-evidence dumps); a no-op when the variable is unset.
    fn dump_frame(name: &str) {
        let Ok(dir) = std::env::var("QUAKE_DUMP_DIR") else { return };
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            let (w, h) = (a.render_w, a.render_h);
            let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
            for px in a.fb.chunks(4).take(w * h) {
                out.extend_from_slice(&px[..3]);
            }
            let _ = std::fs::write(format!("{dir}/{name}.ppm"), out);
        });
    }

    #[test]
    fn level_exit_runs_the_intermission_then_changelevel() {
        // The faithful end-of-level flow, driven end-to-end through the REAL
        // progs.dat: touching trigger_changelevel runs execute_changelevel (the
        // QC freezes the player on the info_intermission spot and WriteBytes
        // svc_intermission to MSG_ALL), the engine enters intermission mode and
        // draws the stats overlay, and only a button press AFTER
        // intermission_exittime (time+2) runs GotoNextMap -> changelevel(e1m2).
        assert_eq!(boot(), 1);
        set_resolution(320, 200); // debug-build render speed; clamps to the min preset
        close_menu(); // boot opens the menu; buttons are gated while it is up
        let before_shells = player_field("ammo_shells") as i32;

        drive_into_exit();

        // --- The engine is in intermission: camera frozen on the QC-moved
        // player, stats overlay up, status bar hidden.
        walk_mut(|w| {
            assert_eq!(w.intermission, 1, "svc_intermission set cl.intermission = 1");
            // cl.completed_time = cl.time = sv.time (cl_parse.c:939), and
            // SV_SpawnServer starts sv.time at 1.0 — the walk clock (starting at
            // 0) would read at least 1 second LOW here. drive_into_exit returned
            // the moment the latch happened, so the latched value IS the server's
            // current clock.
            assert!(w.completed_time >= 1.0, "completed_time latches sv.time (epoch 1.0)");
            assert_eq!(
                w.completed_time,
                w.server.time(),
                "completed_time = sv.time at the latch (no steps ran since)"
            );
            // execute_changelevel froze the player: MOVETYPE_NONE, modelindex 0,
            // view_ofs zeroed, moved to the info_intermission spot.
            assert_eq!(
                w.server.vm.ent_get_float(w.player, "movetype") as i32,
                0,
                "player frozen MOVETYPE_NONE"
            );
            assert_eq!(
                w.server.vm.ent_get_vector(w.player, "view_ofs"),
                [0.0, 0.0, 0.0],
                "view_ofs zeroed for the intermission camera"
            );
            // The stats the overlay shows come from the QC globals and are sane.
            assert!(
                w.server.vm.gget_float("total_monsters") > 0.0,
                "e1m1 reports a monster total"
            );
        });
        // The QC moved the player to the info_intermission spot (e1m1 has one);
        // its angles came from the spot's mangle via fixangle.
        let pinned = changelevel_trigger_center();
        walk_mut(|w| {
            let org = w.server.vm.ent_get_vector(w.player, "origin");
            assert_ne!(org, pinned, "player moved OFF the exit to the intermission spot");
        });

        // --- Overlay pixels: render the same frozen frame with and without the
        // intermission flag; the plaque/number region (virtual x>=160, y 56..160)
        // is 3-D view in one and Sbar_IntermissionOverlay in the other.
        let (with_overlay, without_overlay) = walk_mut(|w| {
            let a = step_walk(w, 0.0, false, 320, 200).0;
            w.intermission = 0;
            let b = step_walk(w, 0.0, false, 320, 200).0;
            w.intermission = 1;
            (a, b)
        });
        let region_differs = (56..160).any(|y| {
            (160..320).any(|x| with_overlay.rgb[y * 320 + x] != without_overlay.rgb[y * 320 + x])
        });
        assert!(region_differs, "the intermission overlay painted the stats region");
        dump_frame("intermission-e1m1");

        // --- No button: the intermission HOLDS even long past exittime.
        for _ in 0..25 {
            step(0.1);
        }
        walk_mut(|w| {
            assert_eq!(w.intermission, 1, "no button => still at the intermission");
            assert_eq!(w.map_name, "maps/e1m1.bsp", "no level change without a button");
        });

        // --- Attack pressed: IntermissionThink (time >= exittime, button down)
        // runs ExitIntermission -> GotoNextMap -> changelevel("e1m2"); the host
        // drains the pending request and swaps, carrying the inventory parms.
        walk_mut(|w| w.in_attack = true);
        for _ in 0..5 {
            step(0.1);
            if walk_mut(|w| w.map_name.clone()) != "maps/e1m1.bsp" {
                break;
            }
        }
        walk_mut(|w| {
            assert_eq!(w.map_name, "maps/e1m2.bsp", "the exit leads to e1m2");
            assert_eq!(w.intermission, 0, "the new level starts out of intermission");
            w.in_attack = false;
        });
        assert_eq!(
            player_field("ammo_shells") as i32,
            before_shells,
            "spawn parms carried the inventory across the swap"
        );
    }

    #[test]
    fn e1m7_exit_reaches_the_shareware_finale_and_sellscreen() {
        // Episode end: e1m7's exit runs the same intermission, but the SECOND
        // button press (ExitIntermission with intermission_running == 2 and
        // world.model == "maps/e1m7.bsp", cvar("registered") == 0) emits
        // svc_finale + the shareware episode text, and the THIRD press
        // (running == 3, shareware) emits svc_sellscreen — which pops the
        // Help/Ordering menu exactly like Cmd_ExecuteString("help").
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        close_menu();
        console_toggle();
        run_console_line("map e1m7");
        walk_mut(|w| assert_eq!(w.map_name, "maps/e1m7.bsp", "console map swap"));
        assert_eq!(console_visible(), 0, "a successful map command closed the console");
        close_menu(); // the fresh-walk path must not leave the menu gating input

        drive_into_exit();
        walk_mut(|w| assert_eq!(w.intermission, 1));

        // Hold attack: IntermissionThink exits as soon as time passes exittime
        // (time+2), then svc_finale arrives with the episode-end text.
        walk_mut(|w| w.in_attack = true);
        for _ in 0..30 {
            step(0.1);
            if walk_mut(|w| w.intermission) == 2 {
                break;
            }
        }
        walk_mut(|w| {
            assert_eq!(w.intermission, 2, "svc_finale set cl.intermission = 2");
            assert!(
                w.finale_text.starts_with("As the corpse of the monstrous entity"),
                "the shareware episode-1 finale text arrived; got {:?}",
                &w.finale_text[..w.finale_text.len().min(60)]
            );
            assert_eq!(w.map_name, "maps/e1m7.bsp", "the finale shows BEFORE any map change");
        });
        // Let ~1.5s of the slow text reveal pass, then dump the evidence frame.
        walk_mut(|w| w.in_attack = false);
        for _ in 0..15 {
            step(0.1);
        }
        dump_frame("finale-e1m7");

        // Third press (after the finale's exittime = time+1): shareware emits
        // svc_sellscreen; the dispatcher opens the menu on the Help screen.
        walk_mut(|w| w.in_attack = true);
        for _ in 0..30 {
            step(0.1);
            let open = APP.with(|c| c.borrow().as_ref().unwrap().menu.visible);
            if open {
                break;
            }
        }
        APP.with(|c| {
            let b = c.borrow();
            let menu = &b.as_ref().unwrap().menu;
            assert!(menu.visible, "svc_sellscreen popped the menu");
            assert_eq!(
                menu.screen(),
                render::MenuScreen::Help,
                "the sell screen is the Help/Ordering pages"
            );
        });
        walk_mut(|w| w.in_attack = false);
    }

    // =======================================================================

    // Demo parity: the recorded stream drives sound / sbar / viewmodel /
    // lightstyles / damage exactly like live play.
    // =======================================================================

    /// The REAL embedded demo1.dem decodes the full recorded stream the demo
    /// path previously discarded: hundreds of svc_sound one-shots, the
    /// clientdata stats (sbar source), the SU_WEAPON viewmodel index, the
    /// signon lightstyle table, and — via the signon gate — an in-world first
    /// frame (no void-camera intro).
    #[test]
    fn demo1_recorded_stream_carries_sounds_stats_styles_and_viewmodel() {
        let pak = pak().expect("embedded pak");
        let bytes = pak.read_file("demo1.dem").unwrap().expect("demo1.dem in pak");
        let demo = parse_demo(&bytes).expect("demo1 parses");

        // (a) recorded svc_sound events decoded: id's demo1 carries ~595
        // one-shots (gunshots, doors, monster barks). Assert a robust floor.
        let sounds: usize = demo.frames.iter().map(|f| f.sounds.len()).sum();
        assert!(sounds >= 500, "demo1 carries ~595 svc_sound events, got {sounds}");
        // Every event resolved its precache name (S_StartSound's sfx lookup).
        assert!(
            demo.frames.iter().flat_map(|f| &f.sounds).all(|s| !s.sample.is_empty()),
            "every recorded sound resolves a precache name"
        );

        // (d/e) clientdata: the sbar stats are present from the FIRST frame.
        let f0 = &demo.frames[0];
        assert_eq!(f0.client.health, 100, "fresh recorded player");
        assert_eq!(f0.client.ammo, 25);
        assert_eq!(f0.client.shells, 25);
        assert_eq!(f0.client.active_weapon, 1, "IT_SHOTGUN");
        assert_ne!(f0.client.items, 0, "recorded cl.items bits present");

        // (f) viewmodel: STAT_WEAPON resolves through the demo's precache to
        // the shotgun viewmodel.
        assert_eq!(
            demo.model_precache
                .get(f0.client.weapon_model.max(0) as usize)
                .map(|s| s.as_str()),
            Some("progs/v_shot.mdl"),
            "SU_WEAPON -> model_precache -> v_shot.mdl"
        );

        // (c) the recorded lightstyle table: style 0 = 'm' (the steady world)
        // plus the torch-flicker set from the signon.
        assert_eq!(f0.lightstyles.first().map(|s| s.as_str()), Some("m"));
        let nonempty = f0.lightstyles.iter().filter(|s| !s.is_empty()).count();
        assert!(nonempty >= 10, "signon carries the style table, got {nonempty}");

        // (i) no void-camera intro: the signon gate makes frame 0 in-world.
        assert!(f0.entities.len() > 10, "frame 0 renders the level's entities");
        assert_ne!(f0.view_origin, [0.0; 3], "frame 0 camera is in-world");

        // (j) the recorded SU_VELOCITY drives V_CalcBob: the run reaches real
        // ground speed (id's demo1 visibly bobs).
        let maxv = demo
            .frames
            .iter()
            .map(|f| (f.client.velocity[0].powi(2) + f.client.velocity[1].powi(2)).sqrt())
            .fold(0.0f32, f32::max);
        assert!(maxv > 200.0, "recorded velocity shows the player running ({maxv})");
    }

    /// (b) svc_stopsound census: id's shipped demos never send it — the
    /// (entity, channel) stop registry is protocol completeness, exercised by
    /// the synthetic decode test in quake-rs. If a future demo carries stops,
    /// the page's keyed-source registry honours them.
    #[test]
    fn id_demos_never_send_stopsound() {
        let pak = pak().expect("embedded pak");
        for name in ["demo1.dem", "demo2.dem", "demo3.dem"] {
            let bytes = pak.read_file(name).unwrap().expect("demo in pak");
            let demo = parse_demo(&bytes).expect("demo parses");
            let stops: usize = demo.frames.iter().map(|f| f.stop_sounds.len()).sum();
            assert_eq!(stops, 0, "{name} sends no svc_stopsound");
        }
    }

    /// A recorded svc_sound event queues through the SAME `queue_sounds` path
    /// live play uses — once per frame advance (the spawn guard), carrying its
    /// (entity, channel) override key for the page registry.
    #[test]
    fn step_demo_queues_recorded_sounds_through_the_live_path() {
        use quake_rs::demo::{Demo, DemoFrame};

        let plain = |t: f32| DemoFrame { time: t, ..Default::default() };
        let sound_frame = DemoFrame {
            time: 0.05,
            sounds: vec![SoundEvent {
                entity: 5,
                channel: 2,
                sound_index: 1,
                sample: "doors/x.wav".to_string(),
                origin: [64.0, 0.0, 0.0],
                volume: 0.5,
                attenuation: 1.0,
            }],
            ..Default::default()
        };
        let demo = Demo {
            level_name: "test".into(),
            static_sounds: Vec::new(),
            model_precache: vec![String::new(), "maps/test.bsp".into()],
            sound_precache: Vec::new(),
            viewentity: 1,
            frames: vec![plain(0.0), sound_frame, plain(0.10)],
        };
        let mut d = DemoPlay {
            bsp: render::demo_room(),
            palette: [[0u8; 3]; 256],
            demo,
            models: Vec::new(),
            sprites: Vec::new(),
            colormap: None,
            colors: Vec::new(),
            elapsed: 0.0,
            idx: 0,
            particles: ParticleSystem::new(),
            prng: Lcg::new(1),
            last_spawned_idx: usize::MAX,
            beams: Beams::new(),
            beam_scratch: Vec::new(),
            gfx_wad: None,
            conchars: None,
            pic_complete: None,
            pic_inter: None,
            pic_finale: None,
            pak: build_test_pak(&[("sound/doors/x.wav", b"WAVE")]),
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
        };

        reset_queue(); // clears SND_QUEUE + marks audio ready
        let _ = step_demo(&mut d, 0.05, false, 160, 100);
        assert_eq!(d.idx, 1, "advanced onto the sound frame");
        assert_eq!(
            SND_QUEUE.with(|q| q.borrow().len()),
            1,
            "the recorded svc_sound queued exactly once"
        );
        // Lingering on the same frame must not re-queue it.
        let _ = step_demo(&mut d, 0.0001, false, 160, 100);
        assert_eq!(SND_QUEUE.with(|q| q.borrow().len()), 1, "no re-queue while lingering");

        // The pop carries the spatial params + the (entity, channel) key.
        let len = poll_sound();
        assert!(len > 0, "WAV bytes loaded from the pak");
        assert_eq!(sound_volume(), 0.5);
        assert_eq!(sound_entity(), 5, "override key entity");
        assert_eq!(sound_channel(), 2, "override key channel");
        assert_eq!(
            sound_is_view_entity(),
            0,
            "entity 5 is not the recorded view entity (1)"
        );
        SND_QUEUE.with(|q| q.borrow_mut().clear());
        set_audio_ready(0);
    }

    /// A recorded svc_damage drives the SAME flash + view-kick math live play
    /// uses (V_ParseDamage): the deferred blend returned by step_demo carries
    /// the red cshift, and the kick state arms + decays.
    #[test]
    fn step_demo_damage_event_drives_flash_and_kick() {
        use quake_rs::demo::{DamageEvent, Demo, DemoFrame};

        let plain = |t: f32| DemoFrame { time: t, ..Default::default() };
        let dmg_frame = DemoFrame {
            time: 0.05,
            // Attack from straight ahead (+x of a yaw-0 view at the origin).
            damage: vec![DamageEvent { armor: 0, blood: 20, from: [128.0, 0.0, 0.0] }],
            ..Default::default()
        };
        let demo = Demo {
            level_name: "test".into(),
            static_sounds: Vec::new(),
            model_precache: vec![String::new(), "maps/test.bsp".into()],
            sound_precache: Vec::new(),
            viewentity: 0,
            // Trailing frames keep the fade-out steps below from wrapping the
            // loop (a wrap re-spawns the damage frame's events).
            frames: vec![plain(0.0), dmg_frame, plain(0.10), plain(1.0), plain(2.0)],
        };
        let mut d = DemoPlay {
            bsp: render::demo_room(),
            palette: [[0u8; 3]; 256],
            demo,
            models: Vec::new(),
            sprites: Vec::new(),
            colormap: None,
            colors: Vec::new(),
            elapsed: 0.0,
            idx: 0,
            particles: ParticleSystem::new(),
            prng: Lcg::new(1),
            last_spawned_idx: usize::MAX,
            beams: Beams::new(),
            beam_scratch: Vec::new(),
            gfx_wad: None,
            conchars: None,
            pic_complete: None,
            pic_inter: None,
            pic_finale: None,
            pak: build_test_pak(&[]),
            damage_blend: 0.0,
            damage_color: [0, 0, 0],
            v_dmg_time: 0.0,
            v_dmg_roll: 0.0,
            v_dmg_pitch: 0.0,
            oldz: f32::NAN,
            centerprint: None,
            notify: Vec::new(),
            notify_pending: String::new(),
            viewsize: render::VIEWSIZE_DEFAULT,
        };

        let (_img, color, alpha) = step_demo(&mut d, 0.05, false, 160, 100);
        assert_eq!(d.idx, 1, "advanced onto the damage frame");
        // count = max(10, blood*0.5) = 10 -> percent 30, faded by 0.05*150 =
        // 7.5 within the same step (V_UpdatePalette) -> 22.5.
        assert!(
            (d.damage_blend - 22.5).abs() < 1e-3,
            "V_ParseDamage percent 3*count then dt*150 fade, got {}",
            d.damage_blend
        );
        assert_eq!(d.damage_color, [255, 0, 0], "pure-blood red tint");
        assert_eq!(color, [255, 0, 0], "the deferred blend carries the flash");
        assert!(alpha > 0.0, "non-zero blend returned to the dispatcher");
        // The directional kick armed (forward hit -> pitch kick, no roll) and
        // already decayed one step (v_dmg_time -= host_frametime).
        assert!(
            (d.v_dmg_time - (V_KICKTIME - 0.05)).abs() < 1e-3,
            "kick timer armed then decayed by dt"
        );
        assert!(d.v_dmg_roll.abs() < 1e-3, "head-on hit has no roll component");
        assert!(
            (d.v_dmg_pitch - 10.0 * V_KICKPITCH).abs() < 1e-3,
            "pitch kick = count * dot(from, forward) * v_kickpitch"
        );

        // The flash fades out over the following steps and the blend clears.
        for _ in 0..4 {
            let _ = step_demo(&mut d, 0.05, false, 160, 100);
        }
        assert_eq!(d.damage_blend, 0.0, "flash fully faded");
    }

    /// The real boot demo draws the recorded status bar (sbar pixels differ
    /// from the bare scene) and resolves the recorded viewmodel + lightstyles.
    #[test]
    fn build_demo_resolves_viewmodel_hud_and_recorded_styles() {
        let mut d = build_demo().expect("the embedded demo boots");

        // The recorded SU_WEAPON viewmodel parsed (v_shot.mdl).
        let f0 = &d.demo.frames[0];
        let wm = f0.client.weapon_model.max(0) as usize;
        assert!(
            matches!(d.models.get(wm), Some(Some(_))),
            "the recorded viewmodel's Mdl parsed from the pak"
        );
        // The recorded style table reaches the renderer's scale law: style 0
        // is the steady 'm' world (264/256, what the seeded default used to
        // hardcode) and the flicker styles are present.
        let scales = quake_rs::server::lightstyle_scales_at(&f0.lightstyles, f0.time);
        assert!((scales[0] - 264.0 / 256.0).abs() < 1e-6, "style 0 'm'");
        assert!(d.gfx_wad.is_some(), "sbar pics available for the demo HUD");

        // Status bar A/B: one step with the wad, then re-render the SAME frame
        // without it (dt == 0 holds the frame) — the sbar region must differ.
        let (with_hud, _, _) = step_demo(&mut d, 0.016, false, 320, 200);
        d.gfx_wad = None;
        let (without, _, _) = step_demo(&mut d, 0.0, false, 320, 200);
        assert_eq!(with_hud.rgb.len(), without.rgb.len());
        // Quake's sbar is the bottom 24 rows of the 320x200 virtual screen.
        let bar_rows = 24usize;
        let diff = (0..320 * bar_rows)
            .filter(|i| {
                let a = with_hud.rgb[(200 - bar_rows) * 320 + i];
                let b = without.rgb[(200 - bar_rows) * 320 + i];
                a != b
            })
            .count();
        assert!(diff > 500, "the drawn sbar changes the bar region ({diff} px)");
    }

    /// The loop wrap is seam-clean on the REAL demo: fast-forward to the last
    /// frame, take one more step, and the playback lands back on frame 0 —
    /// which, thanks to the parser's signon gate, is an IN-WORLD frame (the
    /// old stream emitted ~1.2 s of void-camera signon frames here).
    #[test]
    fn demo_loop_wrap_lands_on_the_in_world_first_frame() {
        let mut d = build_demo().expect("the embedded demo boots");
        let n = d.demo.frames.len();
        let _ = step_demo(&mut d, 1.0e6, false, 160, 100);
        assert_eq!(d.idx, n - 1, "fast-forwarded to the last frame");
        let (img, _, _) = step_demo(&mut d, 0.05, false, 160, 100);
        assert_eq!(d.idx, 0, "the wrap landed back on frame 0");
        assert!(
            !d.demo.frames[0].entities.is_empty(),
            "frame 0 is the post-signon in-world frame"
        );
        let lit = img
            .rgb
            .iter()
            .filter(|p| p[0] != 0 || p[1] != 0 || p[2] != 0)
            .count();
        assert!(
            lit * 2 > img.rgb.len(),
            "the post-wrap frame renders a real scene ({lit}/{} lit)",
            img.rgb.len()
        );
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

