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

use quake_rs::bsp::{Bsp, NUM_AMBIENTS};
use quake_rs::demo::{parse_demo, Demo};
use quake_rs::dlight::DynamicLights;
use quake_rs::mdl::Mdl;
use quake_rs::pak::Pak;
use quake_rs::particles::{Lcg, ParticleSystem};
use quake_rs::progs::Progs;
use quake_rs::render::{
    self, build_gamma_table, Camera, Console, Menu, MenuAction, MenuPics, MenuSound,
    ModelInstance, Viewmodel, BIND_ATTACK, BIND_BACK, BIND_CENTERVIEW, BIND_CHANGEWEAPON,
    BIND_FORWARD, BIND_JUMP, BIND_LEFT, BIND_LOOKDOWN, BIND_LOOKUP, BIND_MOVEDOWN,
    BIND_MOVELEFT, BIND_MOVERIGHT, BIND_MOVEUP, BIND_RIGHT, BIND_SPEED, BIND_STRAFE,
};
use quake_rs::server::{Server, StaticSound, TempEntityEvent, UserCmd};
use quake_rs::snd::{
    wav_info, AmbientChannels, AMBIENT_FADE_DEFAULT, AMBIENT_LEVEL_DEFAULT, AMBIENT_SAMPLES,
};
use quake_rs::tent::{BeamModel, BeamSegment, Beams};
use quake_rs::wad::Qpic;

static PAK: &[u8] = include_bytes!("../../quake-data/ID1/PAK0.PAK");

const WALK_MAP: &str = "maps/e1m1.bsp";
const DEMO_FILE: &str = "demo1.dem";
/// The default (boot) render resolution. A crisp `960x600` (preset index 4 — must
/// stay a member of [`render::RESOLUTION_PRESETS`] so the Options "Screen size"
/// label can sync to it). The page restores the player's *saved* resolution from
/// `localStorage` over this on load, and the Options menu lets them change it at
/// runtime; the chosen size now persists across boots / New Game / reloads. The
/// menu + HUD auto-scale to whatever size they're drawn into.
const DEFAULT_W: usize = 960;
const DEFAULT_H: usize = 600;
/// Sane bounds for [`set_resolution`] (and the menu presets): the framebuffer is
/// clamped to this envelope and its total pixel count capped so a runaway value
/// cannot allocate gigabytes. `1280*800*4` bytes ≈ 4 MB is the upper bound.
const MIN_W: i32 = 320;
const MAX_W: i32 = 1280;
const MIN_H: i32 = 200;
const MAX_H: i32 = 800;
const MAX_PIXELS: i32 = 1280 * 800;
/// The legacy analog [`set_move`]/[`set_jump`]/[`set_movedown`] scale (`sv_maxspeed`):
/// those exports predate the bindings-driven key path and feed tests/automation;
/// the page's keyboard input goes through [`key_down`]/[`key_up`] and the
/// faithful `cl_*` move cvars below instead.
const SPEED: f32 = 320.0;

// --- client move cvars (cl_input.c registrations + CL_BaseMove/CL_AdjustAngles) ---
// The server clamps wishspeed to sv_maxspeed (320, ported in server.rs), so a
// 400 run is 320 effective on the ground — exactly WinQuake (walk 200, run 320).

/// `cl_forwardspeed`/`cl_backspeed` ("200"): the walking forward/back rate. The
/// Options "Always Run" toggle swaps them 200 <-> 400 (menu.c M_AdjustSliders
/// case 8); the C sets both cvars to the same value there, so one pair suffices.
/// Always Run defaults ON in this port (Menu's DEVIATION note), so the
/// out-of-the-box rate is the 400 run (320 effective under sv_maxspeed).
const CL_FORWARDSPEED_WALK: f32 = 200.0;
const CL_FORWARDSPEED_RUN: f32 = 400.0;
/// `cl_sidespeed` ("350"): the strafe rate — NOT changed by Always Run.
const CL_SIDESPEED: f32 = 350.0;
/// `cl_upspeed` ("200"): the swim up/down rate — NOT changed by Always Run.
const CL_UPSPEED: f32 = 200.0;
/// `cl_movespeedkey` ("2.0"): the `+speed` modifier multiplies every move.
const CL_MOVESPEEDKEY: f32 = 2.0;
/// `cl_yawspeed` ("140") / `cl_pitchspeed` ("150"): keyboard turn/look rates in
/// deg/sec (CL_AdjustAngles).
const CL_YAWSPEED: f32 = 140.0;
const CL_PITCHSPEED: f32 = 150.0;
/// `cl_anglespeedkey` ("1.5"): `+speed` multiplies the keyboard turn rate.
const CL_ANGLESPEEDKEY: f32 = 1.5;
/// `v_centerspeed` ("500", view.c): the pitch-drift rate centerview/lookspring
/// re-level the view at (V_StartPitchDrift seeds cl.pitchvel with it).
const V_CENTERSPEED: f32 = 500.0;

// --- mouse cvars (in_win.c IN_MouseMove) -----------------------------------

/// The port's `m_yaw`/`m_pitch` magnitude: degrees of turn per (browser mouse
/// count x `sensitivity`). DEVIATION (calibration only): the C's m_yaw/m_pitch
/// are 0.022 deg per Windows mickey; browser `movementX` counts aren't mickeys,
/// and this port has always shipped a 0.16 deg/count feel at the default
/// sensitivity 3 — so the constant is 0.16/3. The multiplicative STRUCTURE is
/// the C's exactly: counts x sensitivity x m_yaw — and the m_pitch SIGN is the
/// Invert Mouse toggle (`m_pitch.value < 0`).
const M_YAW_PORT: f32 = 0.16 / 3.0;
const M_PITCH_PORT: f32 = 0.16 / 3.0;
/// `m_side` ("0.8"): sidemove units per (count x sensitivity) when mouse X is
/// routed to strafe (lookstrafe / +strafe). The C's literal value — the result
/// feeds wishspeed, which sv_maxspeed clamps, so calibration is forgiving.
const M_SIDE: f32 = 0.8;
/// `m_forward` ("1.0"): forwardmove units per count while `+strafe` holds mouse
/// Y out of the pitch path (IN_MouseMove's else branch).
const M_FORWARD: f32 = 1.0;

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

/// One frame of bindings-derived keyboard input, computed in `step` from the
/// held-key table + the menu's binding table and consumed by `step_walk` — a
/// port of `CL_BaseMove`/`CL_AdjustAngles` (cl_input.c) over this port's
/// permanently-held key states (`CL_KeyState`'s fractional first-frame impulse
/// timing needs sub-frame key timestamps the page doesn't deliver; held = 1.0).
#[derive(Clone, Copy, Default)]
struct KeyMove {
    /// `cmd->forwardmove` contribution (cl_forwardspeed/cl_backspeed applied,
    /// including the Always-Run swap and `cl_movespeedkey`).
    fwd: f32,
    /// `cmd->sidemove` contribution (cl_sidespeed; +strafe folds the turn keys in).
    side: f32,
    /// `cmd->upmove` contribution (cl_upspeed).
    up: f32,
    /// `+attack` held.
    attack: bool,
    /// `+jump` held.
    jump: bool,
    /// Keyboard turn direction (+1 = `+left`, -1 = `+right`; 0 with `+strafe`
    /// held — CL_AdjustAngles skips the yaw turn then).
    turn: f32,
    /// Keyboard look direction (+1 = `+lookup`, -1 = `+lookdown`).
    look: f32,
    /// `+speed` held (cl_movespeedkey / cl_anglespeedkey modifiers).
    speed: bool,
}

/// Derive this frame's [`KeyMove`] from the page-held keys through the menu's
/// binding table (keys.c `keybindings` consulted by `Key_Event`; move math per
/// `CL_BaseMove` + `CL_AdjustAngles`).
fn derive_key_move(menu: &Menu, held: &[bool; 256]) -> KeyMove {
    // CL_KeyState: 1.0 while any key bound to `cmd` is held.
    let st = |cmd: usize| -> f32 {
        for (k, &h) in held.iter().enumerate() {
            if h && menu.action_for_key(k as u8) == Some(cmd) {
                return 1.0;
            }
        }
        0.0
    };
    let speed = st(BIND_SPEED) > 0.0;
    let strafe = st(BIND_STRAFE) > 0.0;
    // M_AdjustSliders case 8 ("always run") sets cl_forwardspeed AND
    // cl_backspeed together, so one value serves both directions.
    let fwdspeed = if menu.always_run() {
        CL_FORWARDSPEED_RUN
    } else {
        CL_FORWARDSPEED_WALK
    };
    let mut fwd = fwdspeed * st(BIND_FORWARD) - fwdspeed * st(BIND_BACK);
    let mut side = CL_SIDESPEED * (st(BIND_MOVERIGHT) - st(BIND_MOVELEFT));
    if strafe {
        // CL_BaseMove: with +strafe held the turn keys strafe instead.
        side += CL_SIDESPEED * (st(BIND_RIGHT) - st(BIND_LEFT));
    }
    // +moveup or +jump push up (the port has always let Space double as swim-up
    // in water; on land the ground move ignores upmove and +jump still jumps
    // via button2), +movedown sinks — at cl_upspeed, NOT the run speed.
    let jump = st(BIND_JUMP) > 0.0;
    let mut up = CL_UPSPEED * (st(BIND_MOVEUP).max(st(BIND_JUMP)) - st(BIND_MOVEDOWN));
    if speed {
        // CL_BaseMove: the speed key multiplies forward/side/up by
        // cl_movespeedkey.
        fwd *= CL_MOVESPEEDKEY;
        side *= CL_MOVESPEEDKEY;
        up *= CL_MOVESPEEDKEY;
    }
    KeyMove {
        fwd,
        side,
        up,
        attack: st(BIND_ATTACK) > 0.0,
        jump,
        turn: if strafe { 0.0 } else { st(BIND_LEFT) - st(BIND_RIGHT) },
        look: st(BIND_LOOKUP) - st(BIND_LOOKDOWN),
        speed,
    }
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
            // menu's Screen-size preset at it, instead of snapping back to DEFAULT.
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
            // menu's Screen-size preset at it so it's correct when the player
            // next opens Options.
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
            // menu's Screen-size label to it. On the very first load the
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

/// The current render width in pixels (defaults to [`DEFAULT_W`] = 960). The page
/// reads this each frame and resizes its canvas backing store + ImageData when it
/// changes (e.g. after the Options menu picks a different preset), and persists it
/// to `localStorage` so the choice survives a reload.
#[no_mangle]
pub extern "C" fn width() -> i32 {
    APP.with(|c| c.borrow().as_ref().map(|a| a.render_w as i32).unwrap_or(DEFAULT_W as i32))
}
/// The current render height in pixels (defaults to [`DEFAULT_H`] = 600).
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
    ensure_app(|a| {
        a.set_render_size(cw, ch);
        // Keep the Options "Screen size" label pointing at the new size too, so a
        // programmatic set (e.g. the page restoring a saved resolution on load)
        // doesn't leave the menu showing a stale preset.
        a.menu.sync_resolution(cw as i32, ch as i32);
    });
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
    let mut new_size: Option<(usize, usize)> = None;
    let mut slot_action: Option<(bool, usize)> = None; // (is_save, slot)
    ensure_app(|a| {
        if a.menu.visible {
            match a.menu.select() {
                MenuAction::NewGame => start_new_game = true,
                MenuAction::ResolutionChanged => {
                    // Enter on the Options "Screen size" row cycled the preset;
                    // capture the new (clamped) size and resize the framebuffer
                    // after the borrow, exactly like the left/right-arrow path.
                    let (rw, rh) = a.menu.resolution();
                    new_size = Some(clamp_resolution(rw, rh));
                }
                MenuAction::OpenConsole => {
                    // Options "Go to console": select() already closed the menu;
                    // open the drop-down console (Con_ToggleConsole_f).
                    a.console.open = true;
                }
                MenuAction::ResetDefaults => {
                    // Options "Reset to defaults": select() reset the in-menu
                    // cvars; re-read the render-size preset (unchanged here) so the
                    // Screen-size row stays in sync. Sensitivity/volume are read
                    // live by the host each frame, so nothing else to do.
                    a.menu.sync_resolution(a.render_w as i32, a.render_h as i32);
                }
                // Save/Load menu slots -> the Host_Savegame_f/Host_Loadgame_f
                // port, via the same console-command path (`save sN`/`load sN`,
                // the C's "s%i.sav" naming). Executed OUTSIDE this borrow:
                // a load swaps the whole Walk (like start_new_game).
                MenuAction::SaveSlot(i) => slot_action = Some((true, i)),
                MenuAction::LoadSlot(i) => slot_action = Some((false, i)),
                // Closed/Back/None already applied to the menu state inside
                // select(); nothing else for the host to do.
                _ => {}
            }
        }
    });
    if let Some((w, h)) = new_size {
        ensure_app(|a| a.set_render_size(w, h));
    }
    if let Some((is_save, i)) = slot_action {
        // Menu slot -> the same path as the console `save sN` / `load sN`
        // (Host_Savegame_f/Host_Loadgame_f port). Runs outside the borrow:
        // a successful load replaces the Walk.
        let name = format!("s{i}");
        if is_save {
            do_save_command(Some(&name));
        } else {
            do_load_command(Some(&name));
        }
    }
    if start_new_game {
        // Fresh single-player game on the start hub (NEW_GAME_MAP). Rebuild the
        // whole walk — new Server, new connected client — switch to walk mode and
        // leave the menu closed. From the hub the player picks skill + episode
        // (changelevel).
        if let Some(nw) = build_walk_map(render::NEW_GAME_MAP) {
            ensure_app(|a| {
                a.walk = Some(nw);
                a.mode = 0;
                // Reset the menu's NAVIGATION and leave it closed. The player's
                // options and key rebinds SURVIVE New Game: WinQuake's
                // M_SinglePlayer "New Game" just runs `map start` — cvars and
                // keybindings persist (the flagship "rebind keys / set Always
                // Run, then New Game" flow must not lose them).
                a.menu.reset_nav();
                // PRESERVE the chosen resolution across New Game (keep the live
                // framebuffer) and point the menu's Screen-size preset at it,
                // instead of snapping back to DEFAULT — starting a game no longer
                // throws away a menu-picked resolution.
                a.menu.sync_resolution(a.render_w as i32, a.render_h as i32);
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

/// Answer the Quit confirmation prompt "Yes" (the literal `Y` key, `M_Quit_Key`
/// 'y'/'Y'): close the menu (quit to the attract loop). A no-op off the Quit
/// screen, so the page can route a `Y` press here unconditionally while the menu
/// is up. Enter (`menu_select`) on the Quit screen does the same thing.
#[no_mangle]
pub extern "C" fn menu_quit_yes() {
    ensure_app(|a| {
        if a.menu.visible {
            let _ = a.menu.quit_yes();
        }
    });
}

/// Answer the Quit confirmation prompt "No" (the literal `N` key, `M_Quit_Key`
/// 'n'/'N'): back out to the screen the prompt rose from. A no-op off the Quit
/// screen. Escape (`menu_cancel`) on the Quit screen does the same thing.
#[no_mangle]
pub extern "C" fn menu_quit_no() {
    ensure_app(|a| {
        if a.menu.visible {
            let _ = a.menu.quit_no();
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

/// Backspace/Del while the menu is up: on the Customize-controls screen this
/// unbinds the highlighted command (`M_Keys_Key` K_BACKSPACE/K_DEL); on every
/// other screen it is a no-op (the engine gates it).
#[no_mangle]
pub extern "C" fn menu_backspace() {
    ensure_app(|a| {
        if a.menu.visible {
            a.menu.keys_backspace();
        }
    });
}

/// 1 while the Keys screen is waiting for the next key to bind (`bind_grab`,
/// menu.c). The page reads this to route the NEXT raw keypress to
/// [`menu_bind_key`] instead of menu navigation.
#[no_mangle]
pub extern "C" fn menu_bind_grabbing() -> i32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| (a.menu.visible && a.menu.bind_grabbing()) as i32)
            .unwrap_or(0)
    })
}

/// Deliver the grabbed key to the Keys screen (`M_Keys_Key`, the `bind_grab`
/// branch): Quake keynum in `0..256`. Escape cancels, backtick is refused, any
/// other key binds to the highlighted command; the grab ends either way. A
/// no-op when nothing is grabbing.
#[no_mangle]
pub extern "C" fn menu_bind_key(keynum: i32) {
    if !(0..256).contains(&keynum) {
        return;
    }
    ensure_app(|a| {
        if a.menu.visible {
            a.menu.bind_key(keynum as u8);
        }
    });
}

// --- bindings-driven game keys (keys.c Key_Event -> keybindings consult) -----

/// A game key went down, by Quake keynum (keys.h: printable ASCII is itself
/// lowercase; arrows/modifiers take the 128+ block; mouse buttons 200+). The
/// held state feeds the per-frame `CL_BaseMove` derivation through the menu's
/// binding table; the non-`+` commands (`impulse 10`, `centerview`) fire their
/// one-shot here like `Key_Event`'s command dispatch. The page must not route
/// keys here while the menu/console own the keyboard (`key_dest != key_game`) —
/// and the engine gates the one-shots regardless.
#[no_mangle]
pub extern "C" fn key_down(keynum: i32) {
    if !(0..256).contains(&keynum) {
        return;
    }
    ensure_app(|a| {
        a.keys_held[keynum as usize] = true;
        if a.menu.visible || a.console.open {
            return; // key_dest != key_game: no command dispatch.
        }
        match a.menu.action_for_key(keynum as u8) {
            Some(BIND_CHANGEWEAPON) => {
                // "impulse 10": queue the next-weapon impulse once, like the
                // console command (Cbuf -> IN_Impulse).
                if let Some(w) = a.walk.as_mut() {
                    w.next_impulse = 10;
                }
            }
            Some(BIND_CENTERVIEW) => {
                // "centerview" -> V_StartPitchDrift (view.c): seed the drift.
                if let Some(w) = a.walk.as_mut() {
                    if !w.pitch_drift || w.pitch_vel == 0.0 {
                        w.pitch_vel = V_CENTERSPEED;
                        w.pitch_drift = true;
                    }
                }
            }
            _ => {}
        }
    });
}

/// A game key went up, by Quake keynum. Always honoured — even while the
/// menu/console are up — so a key released behind an overlay can never stick
/// held (Key_Event delivers key-ups to `+` commands regardless of key_dest).
#[no_mangle]
pub extern "C" fn key_up(keynum: i32) {
    if !(0..256).contains(&keynum) {
        return;
    }
    ensure_app(|a| {
        a.keys_held[keynum as usize] = false;
    });
}

/// 1 when the engine currently believes Quake keynum `keynum` is held — a
/// read-only verification/debug export (like [`menu_screen_id`]). The browser
/// harness uses it to prove the page's `e.code` punctuation mapping keeps
/// key-down/key-up SYMMETRIC under Shift (press ',', add Shift, release ','
/// must clear keynum 44, even though the release reports `key == '<'` —
/// the C's scancode semantics, in_win.c `scantokey`).
#[no_mangle]
pub extern "C" fn key_is_down(keynum: i32) -> i32 {
    if !(0..256).contains(&keynum) {
        return 0;
    }
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| a.keys_held[keynum as usize] as i32)
            .unwrap_or(0)
    })
}

/// Raw mouse deltas (browser `movementX`/`movementY` counts) — a port of
/// IN_MouseMove (in_win.c): counts scale by the `sensitivity` cvar; mouse X
/// turns yaw, OR strafes (`m_side`) while `lookstrafe` is on or `+strafe` is
/// held; mouse Y drives pitch (sign = Invert Mouse, `m_pitch.value < 0`),
/// clamped 80/-70, OR feeds forwardmove (`m_forward`) while `+strafe` holds it
/// out of the pitch path. Mouse-look is permanent under pointer lock (`+mlook`
/// held), so any motion stops an active pitch drift (V_StopPitchDrift). Gated
/// behind the menu/console like `look`.
#[no_mangle]
pub extern "C" fn mouse_move(dx: f32, dy: f32) {
    ensure_app(|a| {
        if a.menu.visible || a.console.open {
            return;
        }
        if !dx.is_finite() || !dy.is_finite() {
            return;
        }
        // mouse_x *= sensitivity.value (the raw 1..11 cvar, like the C — the
        // 0.16/3 port calibration lives in M_YAW_PORT/M_PITCH_PORT).
        let mx = dx * a.menu.sensitivity();
        let my = dy * a.menu.sensitivity();
        let strafe_held = a
            .keys_held
            .iter()
            .enumerate()
            .any(|(k, &h)| h && a.menu.action_for_key(k as u8) == Some(BIND_STRAFE));
        let lookstrafe = a.menu.lookstrafe();
        let invert = a.menu.invert_mouse();
        if let Some(w) = a.walk.as_mut() {
            // if (in_strafe || (lookstrafe && in_mlook)) sidemove += m_side*mx
            // else viewangles[YAW] -= m_yaw*mx. (+mlook is always held here.)
            if strafe_held || lookstrafe {
                w.mouse_side += M_SIDE * mx;
            } else {
                w.yaw -= M_YAW_PORT * mx;
            }
            // if (in_mlook) V_StopPitchDrift() — every mlook mouse move.
            w.pitch_drift = false;
            w.pitch_vel = 0.0;
            // if (in_mlook && !in_strafe) pitch += m_pitch*my (clamped 80/-70)
            // else forwardmove -= m_forward*my.
            if !strafe_held {
                let m_pitch = if invert { -M_PITCH_PORT } else { M_PITCH_PORT };
                w.pitch = clamp_pitch(w.pitch + m_pitch * my);
            } else {
                w.mouse_fwd -= M_FORWARD * my;
            }
        }
    });
}

/// The pointer lock was released. This port's `+mlook` is permanently held
/// while the pointer is locked, so unlock IS the mlook release — the faithful
/// `lookspring` trigger (`IN_MLookUp`, cl_input.c: when `+mlook` releases and
/// `lookspring.value` is set, `V_StartPitchDrift()` re-centres the view).
#[no_mangle]
pub extern "C" fn pointer_unlocked() {
    ensure_app(|a| {
        if !a.menu.lookspring() {
            return;
        }
        if let Some(w) = a.walk.as_mut() {
            // V_StartPitchDrift (view.c): seed pitchvel, clear nodrift.
            if !w.pitch_drift || w.pitch_vel == 0.0 {
                w.pitch_vel = V_CENTERSPEED;
                w.pitch_drift = true;
            }
        }
    });
}

/// The player's current look pitch in degrees (+down, Quake convention) — a
/// read-only verification/debug export (the browser checks Invert Mouse and
/// lookspring flip/centre the pitch through it). 0 when no walk is live.
#[no_mangle]
pub extern "C" fn player_pitch() -> f32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|a| a.walk.as_ref().map(|w| w.pitch))
            .unwrap_or(0.0)
    })
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

/// The menu screen currently showing, as a stable id — a read-only
/// verification/debug export (the browser checks the screen transitions:
/// Multiplayer opens, Save gates, Video applies). 0 Main, 1 SinglePlayer,
/// 2 Load, 3 Save, 4 Multiplayer, 5 Options, 6 Keys, 7 Video, 8 Help, 9 Quit.
#[no_mangle]
pub extern "C" fn menu_screen_id() -> i32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| match a.menu.screen() {
                render::MenuScreen::Main => 0,
                render::MenuScreen::SinglePlayer => 1,
                render::MenuScreen::Load => 2,
                render::MenuScreen::Save => 3,
                render::MenuScreen::Multiplayer => 4,
                render::MenuScreen::Options => 5,
                render::MenuScreen::Keys => 6,
                render::MenuScreen::Video => 7,
                render::MenuScreen::Help => 8,
                render::MenuScreen::Quit => 9,
            })
            .unwrap_or(0)
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
const IT_INVISIBILITY: i32 = 1 << 19; // Ring of Shadows (524288)
const FL_GODMODE: i32 = 64;
const FL_ONGROUND: i32 = 512;
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
                a.console.println("  save <name>   load <name>");
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

    // `save`/`load` (Host_Savegame_f / Host_Loadgame_f): handled at this level
    // because load replaces the whole Walk (via the page round-trip) and save
    // runs guards that need the App, not just the walk. The C's Cmd_Argc()!=2
    // check covers extra args too, so pass None unless exactly one argument.
    if cmd_lower == "save" {
        do_save_command(if argv.len() == 2 { Some(argv[1]) } else { None });
        return;
    }
    if cmd_lower == "load" {
        do_load_command(if argv.len() == 2 { Some(argv[1]) } else { None });
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
        // Host_Kill_f: suicide through the QuakeC `ClientKill` entry point (the
        // REAL chain: suicide frame, frag penalty, respawn() — which in single
        // player issues localcmd("restart\n")). NOT a health hack: the QuakeC
        // owns the death. The pending restart is honoured HERE, not left for
        // step_walk, because client_frame clears stale requests at the top of
        // each frame — and it matches the C, where the queued "restart" Cbuf
        // text executes right after the kill command itself.
        "kill" => match w.server.client_kill() {
            Ok(true) => {
                if w.server.take_pending_restart() {
                    try_restart(w);
                }
                // No success line of its own: the QuakeC bprints
                // "<netname> suicides" (drained into notify next frame).
            }
            Ok(false) => out.push("Can't suicide -- allready dead!".into()),
            Err(e) => out.push(format!("kill failed: {e}")),
        },
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
            // Navigation-only reset: the C's `map` command never resets cvars or
            // keybindings, so the player's options and rebinds survive here too.
            a.menu.reset_nav();
            // Preserve the player's chosen render resolution across a `map` (the C
            // keeps the video mode): the framebuffer is untouched, and we eagerly
            // point the menu's Screen-size preset at it — same as every other
            // re-boot site — so the Options label is correct the instant the
            // player opens it (not relying on the per-frame sync in step()).
            a.menu.sync_resolution(a.render_w as i32, a.render_h as i32);
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

/// `Host_FilterTime` (host.c): the most a single frame may advance the game —
/// a longer real frame (a hitch, a backgrounded tab) is clamped to 0.1 s of
/// `host_frametime` while `realtime` still takes the whole elapsed time.
const HOST_FRAMETIME_MAX: f32 = 0.1;

/// Advance the active mode by `dt` seconds of REAL elapsed time and render into
/// the framebuffer. `dt` is the raw wall-clock delta since the last frame: like
/// `Host_FilterTime`, it all goes to `realtime`, while the game (world, demo,
/// `host_time`) advances by `host_frametime = min(dt, 0.1)`.
#[no_mangle]
pub extern "C" fn step(dt: f32) {
    // Guard a non-finite / negative dt so both clocks only move forward.
    let real_dt = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
    // Host_FilterTime's upper clamp (its 0.001 floor and 72 fps cap are not
    // modelled: dt = 0 must keep freezing the world for the tests/automation).
    let dt = real_dt.min(HOST_FRAMETIME_MAX);
    ensure_app(|a| {
        // Advance both App clocks — mode-independent, so the menudot spinner and
        // the flashing cursors keep animating over a frozen frame.
        a.realtime += real_dt as f64;
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
                        a.realtime,
                        palette,
                    );
                }
            }
        }

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

        // V_UpdatePalette runs LAST in SCR_UpdateScreen: tint the fully composited
        // frame (3D + HUD + centerprint/notify + menu + console) with the deferred
        // damage/water/powerup blend, matching software Quake's whole-screen palette
        // shift. A zero alpha (no active shift, or the demo path) is a no-op.
        if let Some(img) = img.as_mut() {
            render::apply_blend(img, blend.0, blend.1);
        }

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
    /// The emitting entity + channel (`SND_PickChannel`'s override key). The
    /// page reads these via `sound_entity`/`sound_channel` to keep a registry
    /// of PLAYING sources per `(entity, channel)`, so a NEW sound on a
    /// non-zero channel STOPS the source it overrides (the C "always override
    /// sound from same entity" — channel 0 never overrides), and an
    /// `svc_stopsound` can stop the keyed source (S_StopSound).
    entity: i32,
    channel: i32,
}

impl SndParams {
    const fn zero() -> Self {
        SndParams {
            origin: [0.0; 3],
            volume: 0.0,
            attenuation: 0.0,
            is_view_entity: false,
            entity: 0,
            channel: 0,
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
    /// Pending menu `S_LocalSound`s (menu1/menu2/menu3), drained from the menu
    /// by [`poll_menu_sound`]. Only fills while audio is ready (same
    /// no-backlog rule as `queue_sounds`).
    static MENU_SND_QUEUE: RefCell<Vec<MenuSound>> = const { RefCell::new(Vec::new()) };
    /// The three menu WAV payloads, loaded from the pak once on first use and
    /// cached (keyed [menu1, menu2, menu3]); `None` = not yet tried.
    static MENU_WAVS: RefCell<[Option<Vec<u8>>; 3]> = const { RefCell::new([None, None, None]) };
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
/// playback queue. The silent `misc/null.wav` is skipped, and the queue is
/// capped at 12 so a noisy frame can't grow it without bound.
///
/// `ambience/*` one-shots are real gameplay content and queue like any other
/// sample: trigger_push wind tunnels fire `sound (other, CHAN_AUTO,
/// "ambience/windfly.wav", 1, ATTN_NORM)` (QuakeC `trigger_push_touch`, heard
/// on E1M6). DEVIATION (Web Audio scope): such samples carry a `cue ` loop
/// chunk, which the C mixer would LOOP on the dynamic channel until overridden
/// (`SND_PaintChannels` wraps at `sc->loopstart`); our one-shot source plays
/// it through once.
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
            if name.is_empty() || name == "misc/null.wav" {
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
                entity: ev.entity,
                channel: ev.channel,
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

/// Pop the next queued MENU sound (menu.c's `S_LocalSound` triggers: menu1 on
/// cursor moves, menu2 on enter/select, menu3 on slider adjusts) into the
/// shared sound scratch and return its WAV byte length (0 when none). The page
/// polls this each frame alongside [`poll_sound`] and plays the bytes per
/// `S_LocalSound` semantics (snd_dma.c: `S_StartSound(cl.viewentity, -1, sfx,
/// vec3_origin, 1, 1)` — full volume, centred, no distance falloff; the page's
/// master volume still scales it, like the C mixer's `volume.value`). Works in
/// EVERY mode (the menu overlays the attract demo too). While the page hasn't
/// reported audio running ([`set_audio_ready`]), queued menu sounds are
/// discarded instead — the same no-backlog rule as `queue_sounds`.
#[no_mangle]
pub extern "C" fn poll_menu_sound() -> i32 {
    // Drain the menu's queue into the local one (or the bin, pre-audio).
    let ready = AUDIO_READY.with(|r| *r.borrow());
    ensure_app(|a| {
        let queued = a.menu.take_sounds();
        if ready && !queued.is_empty() {
            MENU_SND_QUEUE.with(|q| {
                let mut q = q.borrow_mut();
                for s in queued {
                    if q.len() < 16 {
                        q.push(s);
                    }
                }
            });
        }
    });
    if !ready {
        return 0;
    }
    let next = MENU_SND_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if q.is_empty() {
            None
        } else {
            Some(q.remove(0))
        }
    });
    let Some(snd) = next else { return 0 };
    let slot = match snd {
        MenuSound::Menu1 => 0,
        MenuSound::Menu2 => 1,
        MenuSound::Menu3 => 2,
    };
    // Load-once cache: the C's S_PrecacheSound holds these three resident.
    let bytes = MENU_WAVS.with(|w| {
        let mut w = w.borrow_mut();
        if w[slot].is_none() {
            // S_LoadSound: sprintf(namebuffer, "sound/%s", s->name).
            w[slot] = pak()
                .and_then(|p| p.read_file(&format!("sound/{}", snd.sample())).ok().flatten());
        }
        w[slot].clone()
    });
    match bytes {
        Some(b) => {
            let len = b.len() as i32;
            SND.with(|s| *s.borrow_mut() = b);
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

/// The emitting entity of the most recent `poll_sound` pop. With
/// [`sound_channel`] this is the `SND_PickChannel` override key: the page
/// keeps its playing one-shot sources in a registry keyed `(entity, channel)`
/// so a later sound on the same non-zero channel STOPS the source it replaces
/// (snd_dma.c: "always override sound from same entity"), and an
/// `svc_stopsound` can stop it (S_StopSound).
#[no_mangle]
pub extern "C" fn sound_entity() -> i32 {
    SND_CUR.with(|p| p.borrow().entity)
}
/// The channel (0..=7) of the most recent `poll_sound` pop; 0 = CHAN_AUTO,
/// which never overrides and is never stopped by key.
#[no_mangle]
pub extern "C" fn sound_channel() -> i32 {
    SND_CUR.with(|p| p.borrow().channel)
}

thread_local! {
    /// Pending `svc_stopsound` stops, packed as the wire short `(entity << 3)
    /// | channel` (S_StopSound's arguments). Pushed by demo playback (the only
    /// current producer — live play's QuakeC stops loops by playing
    /// `misc/null.wav` on the same channel, which the override path handles);
    /// drained by the page via [`poll_stop_sound`] each frame. NOTE: id's own
    /// demo1/2/3 never send svc_stopsound (asserted by a wasm test), so for
    /// the shipped attract loop this stays empty — the plumbing exists for
    /// protocol completeness.
    static STOP_SND_QUEUE: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
}

/// Queue `(entity, channel)` stops for the page (see [`STOP_SND_QUEUE`]).
fn push_stop_sounds(stops: &[(i32, i32)]) {
    if stops.is_empty() {
        return;
    }
    STOP_SND_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        for &(ent, chan) in stops {
            q.push((ent << 3) | (chan & 7));
        }
    });
}

/// Pop the next pending sound STOP as the packed `(entity << 3) | channel`
/// short (`S_StopSound(i >> 3, i & 7)`), or `-1` when none are pending. The
/// page calls this in a loop each frame and `stop()`s the registered source
/// for that `(entity, channel)` key — the Web Audio equivalent of the C
/// zeroing the channel's sfx.
#[no_mangle]
pub extern "C" fn poll_stop_sound() -> i32 {
    STOP_SND_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if q.is_empty() {
            -1
        } else {
            q.remove(0)
        }
    })
}

// ---------------------------------------------------------------------------
// Looping ambient audio: placed static sounds (PF_ambientsound ->
// S_StaticSound) + the four automatic per-leaf ambient channels
// (S_UpdateAmbientSounds). The page keeps a looping Web Audio source per
// static sound (re-spatialized every frame from the listener pose, same
// distance/pan law as the one-shots) and one looping source per audible
// ambient channel (gain driven by `ambient_gain`, centred — the C sets
// leftvol = rightvol). See quake_rs::snd for the faithful control logic.
// ---------------------------------------------------------------------------

/// One registered static (looping) sound awaiting pickup by the page: the WAV
/// bytes, its spatial params, and the loop window `GetWavinfo` found (seconds;
/// `loop_end` 0.0 = loop to the buffer's end, Web Audio's `loopEnd` default).
struct StaticLoop {
    bytes: Vec<u8>,
    params: SndParams,
    loop_start: f32,
    loop_end: f32,
}

/// The C's effective static-sound budget. `S_StaticSound` (snd_dma.c) refuses
/// at `total_channels == MAX_CHANNELS` (128), but `total_channels` starts at
/// `MAX_DYNAMIC_CHANNELS + NUM_AMBIENTS` = 8 + 4 = 12 (`S_Init`), so at most
/// 116 statics ever fit — and `total_channels++` happens BEFORE the
/// load/loop checks, so a registration that then FAILS (missing or unlooped
/// sample) still burns its slot (see [`queue_static_sounds`]).
const MAX_STATIC_SOUNDS: usize = 128 - (8 + 4);

thread_local! {
    /// Static sounds registered by the CURRENT level, awaiting page pickup via
    /// `poll_static_sound`. NOT gated on `AUDIO_READY` (unlike the one-shot
    /// queue): these are persistent registrations, not a backlog — the page
    /// starts the loops whenever its AudioContext comes up.
    static STATIC_QUEUE: RefCell<Vec<StaticLoop>> = const { RefCell::new(Vec::new()) };
    /// Loop window of the entry the most recent `poll_static_sound` popped (or
    /// `load_ambient_sound` loaded), for the `sound_loop_start`/`sound_loop_end`
    /// exports. `(0, 0)` = loop the whole buffer.
    static SND_LOOP: RefCell<(f32, f32)> = const { RefCell::new((0.0, 0.0)) };
    /// Bumped on every level/mode transition (boot, demo boot, New Game, `map`,
    /// changelevel, restart). The page compares it each frame and, on a change,
    /// stops + drops every looping source — the S_StopAllSounds half of a level
    /// change; the new level's registrations then restart them.
    static SOUND_GENERATION: RefCell<i32> = const { RefCell::new(0) };
    /// The four automatic ambient channels' ramp state (S_UpdateAmbientSounds).
    static AMBIENT: RefCell<AmbientChannels> = const { RefCell::new(AmbientChannels::new()) };
    /// The four channels' CURRENT frame volumes (0..=255) as returned by the
    /// last [`AmbientChannels::update`] — the C's per-frame `chan->leftvol =
    /// chan->rightvol = chan->master_vol` (or the silenced `!l` /
    /// ambient-off frames' nothing-at-all). [`ambient_gain`] serves THESE,
    /// not the raw ramp state, so an out-of-world listener actually goes
    /// quiet on the page while `master_vol` is preserved for re-entry.
    static AMBIENT_VOLS: RefCell<[f32; NUM_AMBIENTS]> =
        const { RefCell::new([0.0; NUM_AMBIENTS]) };
}

/// A level/mode transition happened: invalidate every looping source. Mirrors
/// `S_StopAllSounds` (snd_dma.c), which memsets ALL channels — statics and the
/// ambient ramps included — on every server (re)connect. The page notices the
/// new generation and tears its loop nodes down; the engine-side static queue
/// is dropped (a not-yet-picked-up loop from the old level must never start
/// over the new one) and the ambient master_vols restart from silence.
fn bump_sound_generation() {
    SOUND_GENERATION.with(|g| {
        let mut g = g.borrow_mut();
        *g = g.wrapping_add(1);
    });
    STATIC_QUEUE.with(|q| q.borrow_mut().clear());
    AMBIENT.with(|a| *a.borrow_mut() = AmbientChannels::new());
    AMBIENT_VOLS.with(|v| *v.borrow_mut() = [0.0; NUM_AMBIENTS]);
}

/// Load the WAV bytes for each placed static sound and queue them for the
/// page's loop pickup — the `S_StaticSound` (snd_dma.c:620) gate: a sample the
/// pak lacks is dropped, and so is one with no loop point (`sc->loopstart ==
/// -1` -> "Sound %s not looped"). Quake's ambient samples all carry a `cue `
/// loop chunk; one-shots don't, and the C refuses to static-loop them.
fn queue_static_sounds(pak: &Pak, statics: &[StaticSound]) {
    STATIC_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        // The statics' share of the C channel table (`total_channels - 12`).
        // Local to the call: `S_StopAllSounds` resets `total_channels` on
        // every level change, and a level registers its statics exactly once
        // (one drain -> one call, right after the generation bump).
        let mut slots = 0usize;
        for s in statics {
            if slots >= MAX_STATIC_SOUNDS {
                break; // the C's "total_channels == MAX_CHANNELS" refusal
            }
            if s.sample.is_empty() {
                continue; // the C's `if (!sfx) return` — before the slot grab
            }
            // `total_channels++` precedes S_LoadSound and the loop check in
            // the C, so each of the drops below still burns its slot.
            slots += 1;
            // Sample names are relative to "sound/" (S_LoadSound's sprintf),
            // exactly like the one-shot path in `queue_sounds`.
            let path = format!("sound/{}", s.sample);
            let Ok(Some(bytes)) = pak.read_file(&path) else {
                continue;
            };
            let Some(info) = wav_info(&bytes) else {
                continue;
            };
            let Some(loop_start) = info.loop_start else {
                continue; // "Sound %s not looped" — never static-loop a one-shot
            };
            let rate = info.rate.max(1) as f32;
            q.push(StaticLoop {
                bytes,
                params: SndParams {
                    origin: s.origin,
                    volume: s.volume,
                    attenuation: s.attenuation,
                    is_view_entity: false, // statics are placed in the world
                    entity: 0,             // statics carry no override key
                    channel: 0,
                },
                loop_start: loop_start as f32 / rate,
                loop_end: info.samples as f32 / rate,
            });
        }
    });
}

/// The current sound generation. The page reads this every frame; when it
/// changes, every looping source (static + ambient) is stopped and rebuilt
/// from the new level's registrations (see [`bump_sound_generation`]).
#[no_mangle]
pub extern "C" fn sound_generation() -> i32 {
    SOUND_GENERATION.with(|g| *g.borrow())
}

/// Pop the next registered static (looping) sound into the scratch buffer and
/// return its byte length (0 when none are pending). Mirrors `poll_sound`: the
/// page reads the bytes via `sound_ptr()` and the spatial params via
/// `sound_origin_*`/`sound_volume`/`sound_attenuation` (stashed exactly like a
/// one-shot pop), plus the loop window via `sound_loop_start`/`sound_loop_end`
/// — then starts a LOOPING source it re-spatializes every frame.
#[no_mangle]
pub extern "C" fn poll_static_sound() -> i32 {
    let next = STATIC_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if q.is_empty() {
            None
        } else {
            Some(q.remove(0))
        }
    });
    match next {
        Some(sl) => {
            let len = sl.bytes.len() as i32;
            SND.with(|s| *s.borrow_mut() = sl.bytes);
            SND_CUR.with(|p| *p.borrow_mut() = sl.params);
            SND_LOOP.with(|l| *l.borrow_mut() = (sl.loop_start, sl.loop_end));
            len
        }
        None => 0,
    }
}

/// Loop start of the most recent `poll_static_sound`/`load_ambient_sound`, in
/// SECONDS (the `cue ` chunk's sample offset over the WAV rate — sample-rate
/// independent, so the page can hand it straight to `AudioBufferSourceNode.
/// loopStart` no matter what rate `decodeAudioData` resampled to).
#[no_mangle]
pub extern "C" fn sound_loop_start() -> f32 {
    SND_LOOP.with(|l| l.borrow().0)
}

/// Loop end in seconds of the most recent `poll_static_sound`/
/// `load_ambient_sound` (`GetWavinfo`'s `info.samples` over the rate; this is
/// the full data length unless a `LIST`/`mark` chunk declared a shorter loop).
/// 0.0 means "to the buffer's end" — Web Audio's `loopEnd` default.
#[no_mangle]
pub extern "C" fn sound_loop_end() -> f32 {
    SND_LOOP.with(|l| l.borrow().1)
}

/// Load ambient channel `ch`'s sample (`S_Init`: 0 = `ambience/water1.wav`,
/// 1 = `ambience/wind2.wav`) into the scratch buffer, returning its byte
/// length; the loop window lands in `sound_loop_start`/`sound_loop_end` like a
/// static pop. Returns 0 for channels the C never loaded (2 = slime, 3 = lava
/// have a NULL `ambient_sfx`) and for out-of-range/missing samples. The page
/// calls this once per audible channel, starts a centred looping source at
/// gain 0, and drives the gain from `ambient_gain` every frame.
#[no_mangle]
pub extern "C" fn load_ambient_sound(ch: i32) -> i32 {
    let Some(Some(name)) = usize::try_from(ch)
        .ok()
        .and_then(|c| AMBIENT_SAMPLES.get(c).copied().map(Some))
    else {
        return 0;
    };
    let Some(name) = name else { return 0 };
    let Some(p) = pak() else { return 0 };
    let Ok(Some(bytes)) = p.read_file(&format!("sound/{name}")) else {
        return 0;
    };
    let Some(info) = wav_info(&bytes) else { return 0 };
    let rate = info.rate.max(1) as f32;
    SND_LOOP.with(|l| {
        *l.borrow_mut() = (
            info.loop_start.unwrap_or(0) as f32 / rate,
            info.samples as f32 / rate,
        )
    });
    let len = bytes.len() as i32;
    SND.with(|s| *s.borrow_mut() = bytes);
    len
}

/// Ambient channel `ch`'s volume THIS frame in `0.0..=1.0` (the value
/// [`AmbientChannels::update`] returned, over the C's 255 scale — see
/// [`AMBIENT_VOLS`]). The page multiplies by its master volume and writes it
/// to the channel's gain node every frame — both sides of the C's
/// `chan->leftvol = chan->rightvol = chan->master_vol` (ambients are centred,
/// never panned or distance-attenuated). 0.0 on the frames the C silenced
/// outright (listener outside the world, `ambient_level` 0).
#[no_mangle]
pub extern "C" fn ambient_gain(ch: i32) -> f32 {
    let Ok(c) = usize::try_from(ch) else { return 0.0 };
    AMBIENT_VOLS.with(|v| v.borrow().get(c).copied().unwrap_or(0.0)) / 255.0
}

/// Ramp the four ambient channels toward `leaf_levels` and publish the frame's
/// returned volumes for [`ambient_gain`] — `update`'s return is the ONLY place
/// the C's silenced `!l`/ambient-off frames differ from the ramp state, so it
/// must be what the page hears.
fn ramp_ambient_channels(leaf_levels: Option<&[u8; NUM_AMBIENTS]>, frametime: f32) {
    let vols = AMBIENT.with(|a| {
        a.borrow_mut().update(
            leaf_levels,
            frametime,
            AMBIENT_LEVEL_DEFAULT,
            AMBIENT_FADE_DEFAULT,
        )
    });
    AMBIENT_VOLS.with(|v| *v.borrow_mut() = vols);
}

/// One frame of `S_UpdateAmbientSounds` for the listener standing at `eye` in
/// `bsp`: look up the view leaf and ramp the four ambient channels toward its
/// `ambient_level[]` targets. Called from both `step_walk` and `step_demo`
/// (the C runs it from `S_Update` regardless of game/demo mode). A listener
/// outside the world (no leaf) silences the channels without resetting the
/// ramp, exactly like the C's `!l` branch.
fn update_ambient_channels(bsp: &Bsp, eye: [f32; 3], dt: f32) {
    let frametime = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
    let leaf_levels = render::point_in_leaf(bsp, eye)
        .and_then(|li| bsp.leafs.get(li))
        .map(|l| l.ambient_level);
    ramp_ambient_channels(leaf_levels.as_ref(), frametime);
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
// Savegame persistence bridge (page-owned localStorage)
//
// The C's Host_Savegame_f/Host_Loadgame_f read and write .sav FILES; in the
// browser the page owns persistence (localStorage), so the engine speaks text
// through the same shared-scratch-buffer style the sound path uses:
//
//  * `save <name>` (console) runs the C's guards, builds the .sav text via
//    Server::write_savegame, and queues (filename, text); the page polls
//    `poll_save()` each frame, reads the name/text out of linear memory, and
//    persists them under a per-name localStorage key. A storage failure
//    reports back through `save_store_failed()` (the async stand-in for the
//    C's synchronous "ERROR: couldn't open.").
//  * `load <name>` (console) prints the C's "Loading game from ..." line and
//    queues a request; the page polls `poll_load_request()`, fetches the
//    stored text, writes it into the wasm scratch via `sav_alloc()` +
//    linear-memory copy, then calls `load_game()`. `load_failed()` reports
//    the C's "ERROR: couldn't open." when the key does not exist OR when
//    `sav_alloc` rejects an oversized value (NULL: the page must not copy).
//  * `extract_save_comment()` parses a stored .sav from the same scratch and
//    returns its comment (underscores back to spaces, M_ScanSaves) for the
//    Load/Save menu slot listings.
// ---------------------------------------------------------------------------

thread_local! {
    /// Completed saves awaiting page pickup: `(filename, .sav text)` pairs.
    static SAVE_QUEUE: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
    /// The save `poll_save()` popped, pinned for the ptr/len exports.
    static SAVE_CUR: RefCell<(String, String)> =
        const { RefCell::new((String::new(), String::new())) };
    /// A pending load: the filename the page should fetch from localStorage.
    static LOAD_REQUEST: RefCell<Option<String>> = const { RefCell::new(None) };
    /// The request `poll_load_request()` popped, pinned for the ptr export.
    static LOAD_REQ_CUR: RefCell<String> = const { RefCell::new(String::new()) };
    /// Page->wasm scratch: stored .sav text handed back for `load_game()` /
    /// `extract_save_comment()` (the inbound twin of the sound scratch).
    static SAV_BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    /// The comment `extract_save_comment()` produced, pinned for its ptr export.
    static SAVE_COMMENT: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Pop the next completed save into the pinned slot and return its TEXT byte
/// length (0 = queue empty). The page then reads `save_name_*` + `save_text_ptr`.
#[no_mangle]
pub extern "C" fn poll_save() -> i32 {
    SAVE_QUEUE.with(|q| {
        let Some(item) = q.borrow_mut().pop() else {
            return 0;
        };
        let len = item.1.len() as i32;
        SAVE_CUR.with(|c| *c.borrow_mut() = item);
        len
    })
}

/// Byte length of the popped save's filename.
#[no_mangle]
pub extern "C" fn save_name_len() -> i32 {
    SAVE_CUR.with(|c| c.borrow().0.len() as i32)
}

/// Pointer to the popped save's filename bytes.
#[no_mangle]
pub extern "C" fn save_name_ptr() -> *const u8 {
    SAVE_CUR.with(|c| c.borrow().0.as_ptr())
}

/// Pointer to the popped save's .sav text bytes (length = `poll_save()`'s return).
#[no_mangle]
pub extern "C" fn save_text_ptr() -> *const u8 {
    SAVE_CUR.with(|c| c.borrow().1.as_ptr())
}

/// The page failed to persist the popped save (localStorage threw — quota or
/// privacy mode). The C fails synchronously with "ERROR: couldn't open."
/// before writing; persistence here is asynchronous, so the error arrives a
/// frame after the optimistic "done." (documented deviation).
#[no_mangle]
pub extern "C" fn save_store_failed() {
    ensure_app(|a| {
        a.console
            .println("ERROR: couldn't store savegame (localStorage full?)");
    });
}

/// Pop a pending load request and return the filename's byte length (0 = none).
#[no_mangle]
pub extern "C" fn poll_load_request() -> i32 {
    LOAD_REQUEST.with(|r| {
        let Some(name) = r.borrow_mut().take() else {
            return 0;
        };
        let len = name.len() as i32;
        LOAD_REQ_CUR.with(|c| *c.borrow_mut() = name);
        len
    })
}

/// Pointer to the popped load request's filename bytes.
#[no_mangle]
pub extern "C" fn load_request_ptr() -> *const u8 {
    LOAD_REQ_CUR.with(|c| c.borrow().as_ptr())
}

/// The page found no stored save under the requested name: the C's fopen
/// failure path, `Con_Printf("ERROR: couldn't open.\n")`.
#[no_mangle]
pub extern "C" fn load_failed() {
    ensure_app(|a| a.console.println("ERROR: couldn't open."));
}

/// Hard cap on the inbound .sav scratch (a real save is ~100-400 KB; 8 MB is
/// far past any legitimate file) so a hostile length can't balloon memory.
const SAV_BUF_MAX: i32 = 8 * 1024 * 1024;

/// Resize the inbound .sav scratch to `len` bytes and return its pointer; the
/// page copies the stored text in, then calls `load_game()` /
/// `extract_save_comment()`. An out-of-range `len` (negative, or past the
/// 8 MB cap) FAILS CLOSED: the scratch is emptied and NULL comes back, and
/// the page must honour the rejection (skip the copy, report `load_failed`).
/// Returning any real pointer for a length we did not allocate would invite
/// the caller to write `len` bytes through it — the exact wild write into
/// linear memory the cap exists to prevent.
#[no_mangle]
pub extern "C" fn sav_alloc(len: i32) -> *mut u8 {
    SAV_BUF.with(|b| {
        let mut b = b.borrow_mut();
        b.clear();
        if !(0..=SAV_BUF_MAX).contains(&len) {
            return std::ptr::null_mut();
        }
        b.resize(len as usize, 0);
        b.as_mut_ptr()
    })
}

/// Load the game whose .sav text the page placed in the scratch buffer
/// (`Host_Loadgame_f`'s post-fopen half). On success the new walk replaces
/// the current mode (1); on any parse/load failure the RUNNING GAME IS LEFT
/// INTACT and the error prints to the console (0) — the C `Sys_Error`ed on a
/// malformed save; we degrade (documented deviation).
#[no_mangle]
pub extern "C" fn load_game() -> i32 {
    let bytes = SAV_BUF.with(|b| std::mem::take(&mut *b.borrow_mut()));
    let text = String::from_utf8_lossy(&bytes).into_owned();
    match build_walk_savegame(&text) {
        Ok(nw) => {
            ensure_app(|a| {
                a.walk = Some(nw);
                a.mode = 0;
                // The loaded game starts playing: close the console + menu
                // (the same post-swap treatment as the console `map` command).
                a.console.open = false;
                a.menu = Menu::new();
                a.menu.sync_resolution(a.render_w as i32, a.render_h as i32);
            });
            1
        }
        Err(msg) => {
            ensure_app(|a| a.console.println(msg));
            0
        }
    }
}

/// Parse the .sav text in the scratch buffer and pin its comment (underscores
/// converted back to spaces, like the C menu's `M_ScanSaves`) for the slot
/// listings; returns the comment's byte length, or 0 for an unparseable text.
#[no_mangle]
pub extern "C" fn extract_save_comment() -> i32 {
    let bytes = SAV_BUF.with(|b| std::mem::take(&mut *b.borrow_mut()));
    let text = String::from_utf8_lossy(&bytes);
    let Ok(sg) = quake_rs::save::parse_savegame(&text) else {
        return 0;
    };
    let comment = quake_rs::save::comment_for_display(&sg.comment);
    let len = comment.len() as i32;
    SAVE_COMMENT.with(|c| *c.borrow_mut() = comment);
    len
}

/// Pointer to the comment bytes `extract_save_comment()` produced.
#[no_mangle]
pub extern "C" fn save_comment_ptr() -> *const u8 {
    SAVE_COMMENT.with(|c| c.borrow().as_ptr())
}

/// MERGE SEAM (Load/Save menu <- localStorage): assign menu slot `slot`'s
/// comment from the savegame text the page just placed in the scratch via
/// [`sav_alloc`] (one stored `.sav` per call). An empty/absent/unparseable
/// buffer marks the slot unused (`"--- UNUSED SLOT ---"` in M_Load_Draw).
/// The page refreshes all 12 slots at boot and after every persisted save,
/// so the menu's listings always mirror what localStorage actually holds.
#[no_mangle]
pub extern "C" fn menu_set_save_comment(slot: i32) {
    let Ok(slot) = usize::try_from(slot) else { return };
    let bytes = SAV_BUF.with(|b| std::mem::take(&mut *b.borrow_mut()));
    let comment = if bytes.is_empty() {
        String::new()
    } else {
        let text = String::from_utf8_lossy(&bytes);
        match quake_rs::save::parse_savegame(&text) {
            Ok(sg) => quake_rs::save::comment_for_display(&sg.comment),
            Err(_) => String::new(),
        }
    };
    ensure_app(|a| a.menu.set_save_comment(slot, comment.clone()));
}

/// `COM_DefaultExtension` (common.c): append `ext` unless the last path
/// component already carries a `.` extension.
fn default_extension(path: &str, ext: &str) -> String {
    let last = path.rsplit('/').next().unwrap_or(path);
    if last.contains('.') {
        path.to_string()
    } else {
        format!("{path}{ext}")
    }
}

/// `Host_Savegame_f` (host_cmd.c), console half: run the C's guard sequence
/// (exact messages, same order — minus `cmd_source`/multiplayer, which don't
/// exist in this single-player shell), then queue the .sav text for the page.
/// `name` is `argv[1]` (`None` reproduces the C's `Cmd_Argc() != 2` usage
/// message at its position in the sequence). Also the host-side entry the
/// Save menu's `MenuAction::SaveSlot(i)` will call with `"s<i>"`.
fn do_save_command(name: Option<&str>) {
    ensure_app(|a| {
        // if (!sv.active) — no live single-player world (demo/attract mode).
        let playing = a.mode == 0 && a.walk.is_some();
        if !playing {
            a.console.println("Not playing a local game.");
            return;
        }
        if a.walk.as_ref().is_some_and(|w| w.intermission != 0) {
            a.console.println("Can't save in intermission.");
            return;
        }
        // (svs.maxclients != 1 — "Can't save multiplayer games." — is
        // unreachable here: this shell is single-client by construction.)
        let Some(name) = name.filter(|s| !s.is_empty()) else {
            a.console.println("save <savename> : save a game");
            return;
        };
        if name.contains("..") {
            a.console.println("Relative pathnames are not allowed.");
            return;
        }
        let Some(w) = a.walk.as_ref() else { return };
        if w.server.player_health() <= 0.0 {
            a.console.println("Can't savegame with a dead player");
            return;
        }
        let fname = default_extension(name, ".sav");
        a.console.println(format!("Saving game to {fname}..."));
        let text = w.server.write_savegame();
        SAVE_QUEUE.with(|q| q.borrow_mut().push((fname, text)));
        // The C prints "done." after its synchronous fwrite; the page's
        // localStorage write happens next frame and reports a failure via
        // save_store_failed() (documented deviation).
        a.console.println("done.");
    });
}

/// `Host_Loadgame_f` (host_cmd.c), console half: print the C's status line and
/// queue the request; the page fetches the stored text and calls back into
/// `load_game()` (or `load_failed()`). Also the host-side entry the Load
/// menu's `MenuAction::LoadSlot(i)` will call with `"s<i>"`.
fn do_load_command(name: Option<&str>) {
    let Some(name) = name.filter(|s| !s.is_empty()) else {
        ensure_app(|a| a.console.println("load <savename> : load a game"));
        return;
    };
    // (cls.demonum = -1 — "stop demo loop in case this fails" — has no
    // equivalent: the attract demo keeps idling until the swap commits.)
    let fname = default_extension(name, ".sav");
    ensure_app(|a| a.console.println(format!("Loading game from {fname}...")));
    LOAD_REQUEST.with(|r| *r.borrow_mut() = Some(fname));
}

/// `Host_Loadgame_f`'s post-fopen half: parse the header, spawn the named map,
/// and rebuild a [`Walk`] around [`Server::load_savegame`]'s reconstructed
/// world. Errors return the console message to print (the C's where it has
/// one); the caller leaves the current game untouched on `Err`.
fn build_walk_savegame(text: &str) -> Result<Walk, String> {
    use quake_rs::save::{parse_savegame, SAVEGAME_VERSION};

    let sg = parse_savegame(text).map_err(|e| e.to_string())?;
    if sg.version != SAVEGAME_VERSION {
        // Con_Printf ("Savegame is version %i, not %i\n", ...)
        return Err(format!(
            "Savegame is version {}, not {}",
            sg.version, SAVEGAME_VERSION
        ));
    }
    let couldnt = || "Couldn't load map".to_string(); // SV_SpawnServer failure
    let pak = pak().ok_or_else(couldnt)?;
    let read = |n: &str| pak.read_file(n).ok().flatten();
    let map = format!("maps/{}.bsp", sg.map_name);
    let map_bytes = read(&map).ok_or_else(couldnt)?;
    let sim_bsp = Bsp::parse(&map_bytes).map_err(|_| couldnt())?;
    let render_bsp = Bsp::parse(&map_bytes).map_err(|_| couldnt())?;
    let progs_bytes = read("progs.dat").ok_or_else(couldnt)?;
    let progs = Progs::parse(&progs_bytes).map_err(|e| e.to_string())?;

    // The engine-side load: header -> SV_SpawnServer (map spawn functions DO
    // run, rebuilding precaches; see save.rs) -> lightstyles -> globals ->
    // edicts -> sv.time/spawn_parms. No entrance script, no signon settle.
    let mut server =
        Server::load_savegame(sim_bsp, progs, Some(pak.clone()), text).map_err(|e| e.to_string())?;
    let player = server.player_edict();

    // The save's spawn parms are the level-ENTRY parms (svs.clients->
    // spawn_parms): a respawn on the loaded level restores the state the
    // player entered it with, exactly like an uninterrupted session.
    let entry_parms = sg.spawn_parms;

    // View angles from the loaded player's v_angle. DEVIATION: the C's
    // Host_Spawn_f sends an svc_setangle built from ent->v.angles (the model
    // angles, whose pitch is the C's -v_angle/3 quirk, roll forced 0 — "never
    // send a roll angle, because savegames can catch the server expecting the
    // client to correct it"); restoring v_angle directly gives back the exact
    // view the player saved with, roll-free here too (this shell has no
    // persistent roll state).
    let v_angle = server.vm.ent_get_vector(player, "v_angle");
    let yaw = v_angle[1];
    let pitch = clamp_pitch(v_angle[0]);

    // Capture this load's placed ambient loops (registered while the map's
    // spawn functions re-ran inside load_savegame) BEFORE the server moves
    // into the Walk; committed to the page only after assembly succeeds.
    let statics = server.drain_static_sounds();

    let mut w = assemble_walk(
        pak,
        map,
        server,
        player,
        entry_parms,
        render_bsp,
        yaw,
        pitch,
    )
    .ok_or_else(couldnt)?;

    // Committed: tear down the previous level/mode's looping audio, start this
    // level's, and drop one-shot events the load's spawn + settle ticks queued
    // (same treatment as every other walk-building path).
    bump_sound_generation();
    queue_static_sounds(&w.pak, &statics);
    let _ = w.server.drain_sounds();
    let _ = w.server.drain_particles();
    let _ = w.server.drain_temp_entities();
    let _ = w.server.drain_messages();
    let _ = w.server.drain_svc_events();
    Ok(w)
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
/// 1024-particle [`ParticleSystem::spawn_explosion`], impact types a
/// `R_RunParticleEffect`-style burst with the matching colour/count, splashes a
/// small upward burst, and beams nothing. Returns `Some(sound_name)` for the types
/// that play a sound (explosions -> r_exp3; spike/super-spike -> tink1/ric*; wizard
/// -> wizard/hit; knight -> hknight/hit), else `None` (gunshot/splashes/beams).
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
        // Spike/super-spike (nailgun, Ogre/Knight nails) wall impact: the dust burst
        // then a ricochet sound — tink1 4/5 of the time, else ric1/ric2/ric3
        // (CL_ParseTEnt). The rng draw follows spawn_burst to keep C's ordering.
        TE_SPIKE | TE_SUPERSPIKE => {
            let count = if ev.te_type == TE_SPIKE { 10 } else { 20 };
            particles.spawn_burst(ev.pos, [0.0; 3], 0, count, now, rng);
            Some(if rng.next_range(5) != 0 {
                "weapons/tink1.wav"
            } else {
                match rng.next_range(4) {
                    1 => "weapons/ric1.wav",
                    2 => "weapons/ric2.wav",
                    _ => "weapons/ric3.wav",
                }
            })
        }
        // Bullet impact: dust only, NO sound (CL_ParseTEnt plays nothing for TE_GUNSHOT).
        TE_GUNSHOT => {
            particles.spawn_burst(ev.pos, [0.0; 3], 0, 20, now, rng);
            None
        }
        // Scrag (wizard) spike impact -> wizard/hit.wav.
        TE_WIZSPIKE => {
            particles.spawn_burst(ev.pos, [0.0; 3], 20, 30, now, rng);
            Some("wizard/hit.wav")
        }
        // Hell-knight spike impact -> hknight/hit.wav.
        TE_KNIGHTSPIKE => {
            particles.spawn_burst(ev.pos, [0.0; 3], 226, 20, now, rng);
            Some("hknight/hit.wav")
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
    // SV_SpawnServer: world.model + the mapname global, before the entities load.
    ns.set_map_name(&map_file);
    // Restore the carried serverflags onto the new server BEFORE spawning its
    // entities, mirroring the C (SV_SpawnServer restores svs.serverflags before
    // ED_LoadFromFile), so the new level's worldspawn — which reads serverflags
    // to light up the runes the player already holds — and the reconnecting
    // client both observe the carried bits. A no-op if the progs lacks the
    // global.
    ns.set_serverflags(serverflags);
    ns.set_skill(skill as f32);
    // Discard stale static-sound registrations (a previously failed spawn's)
    // so the drain after spawn_entities is exactly this level's.
    let _ = ns.drain_static_sounds();
    if ns.spawn_entities().is_err() {
        return;
    }
    // Capture the new level's placed ambient loops now (registered during
    // spawn_entities); committed to the page only once the swap succeeds below.
    let statics = ns.drain_static_sounds();
    let Ok(player) = ns.connect_client_with_parms(parms) else { return };
    // The carried inventory at the start of the NEW level becomes its entry parms,
    // so a respawn on this level restores the state the player arrived with.
    let entry_parms = ns.save_spawn_parms();
    // The C's signon physics frames (see build_walk_map): settle the arriving
    // player onto the floor before the new level's frame 0 renders. Any events
    // these ticks queue are dropped by the post-swap drains below (the C's
    // client misses tick 1's datagram sounds while not yet `spawned`; tick 2's
    // are technically deliverable there — dropping both is a deliberate,
    // inaudible-on-id-maps simplification, identical across all three paths).
    ns.run_signon_frames();

    // Commit the swap. From here nothing can fail.
    let (_spawn, yaw) =
        player_start(&render_bsp.entities).unwrap_or(([0.0, 0.0, 0.0], w.yaw));
    w.server = ns;
    w.bsp = render_bsp;
    w.player = player;
    w.entry_parms = entry_parms;
    w.map_name = map_file;
    w.yaw = yaw;
    w.pitch = 0.0;

    // New level, clean slate: drop the old level's particles / dynamic lights /
    // beams (CL_ClearState memsets cl_beams) and reset the animation clock so
    // liquids/sky restart from zero.
    w.particles = ParticleSystem::new();
    w.dlights = DynamicLights::new();
    w.trail_org.clear();
    w.beams.clear();
    // Clear the on-screen text overlay on level load (SCR_BeginLoadingPlaque calls
    // Con_ClearNotify + scr_centertime_off=0): drop the half-built line AND the
    // already-flushed notify lines + the centerprint. Their expiry is an ABSOLUTE
    // clock value, and the clock resets to 0 below, so a stale "You got the Quad!"
    // would otherwise linger over the new level for old-clock seconds.
    w.notify_pending.clear();
    w.notify.clear();
    w.centerprint = None;
    w.clock = 0.0;
    // Reset the screen-blend state so the level change does not flash red.
    w.damage_blend = 0.0;
    w.last_health = f32::NAN;
    w.last_armor = f32::NAN;
    // Reset stair-step view smoothing so the new spawn doesn't glide from old Z.
    w.oldz = f32::NAN;
    // CL_ClearState: the new level starts OUT of intermission (cl.intermission=0)
    // with no stale finale text or sellscreen request.
    w.intermission = 0;
    w.completed_time = 0.0;
    w.finale_text.clear();
    w.finale_start = 0.0;
    w.pending_sellscreen = false;
    // Drop any events the *outgoing* server queued (the new server starts fresh).
    let _ = w.server.drain_sounds();
    let _ = w.server.drain_particles();
    let _ = w.server.drain_temp_entities();
    let _ = w.server.drain_messages();
    // Looping audio: stop the OLD level's loops (S_StopAllSounds on changelevel)
    // and hand the page the NEW level's placed ambient loops + a fresh ambient
    // ramp, captured above right after spawn_entities.
    bump_sound_generation();
    queue_static_sounds(&w.pak, &statics);
    let _ = w.server.drain_svc_events();
}

/// Single-player respawn: reload the CURRENT level fresh and reconnect the player
/// with the level-ENTRY spawn parms (the state they arrived with). Ported from the
/// engine running QuakeC's `localcmd("restart\n")` — a dead player who presses a
/// button restarts the map. A dead player's own state is useless (health 0, dropped
/// inventory), so `entry_parms` (captured at level entry) is what's restored,
/// matching how `restart` works in id's single-player. A read/parse failure leaves
/// the (dead) level running rather than crashing.
fn try_restart(w: &mut Walk) {
    let serverflags = w.server.serverflags();
    let skill = w.server.skill();
    let read = |n: &str| w.pak.read_file(n).ok().flatten();
    let Some(map_bytes) = read(&w.map_name) else { return };
    let Ok(sim_bsp) = Bsp::parse(&map_bytes) else { return };
    let Ok(render_bsp) = Bsp::parse(&map_bytes) else { return };
    let Some(progs_bytes) = read("progs.dat") else { return };
    let Ok(progs) = Progs::parse(&progs_bytes) else { return };

    let Ok(mut ns) = Server::with_pak(sim_bsp, progs, Some(w.pak.clone())) else { return };
    // SV_SpawnServer: world.model + the mapname global, before the entities load.
    ns.set_map_name(&w.map_name);
    ns.set_serverflags(serverflags);
    ns.set_skill(skill as f32);
    // Static-loop bookkeeping mirrors try_changelevel: discard stale
    // registrations, spawn, capture this (re)load's own.
    let _ = ns.drain_static_sounds();
    if ns.spawn_entities().is_err() {
        return;
    }
    let statics = ns.drain_static_sounds();
    let Ok(player) = ns.connect_client_with_parms(w.entry_parms) else { return };
    // The C's signon physics frames (see build_walk_map): settle the respawned
    // player onto the floor before the restarted level's frame 0 renders.
    ns.run_signon_frames();

    // Commit the reload (nothing below can fail).
    let (_spawn, yaw) =
        player_start(&render_bsp.entities).unwrap_or(([0.0, 0.0, 0.0], w.yaw));
    w.server = ns;
    w.bsp = render_bsp;
    w.player = player;
    w.yaw = yaw;
    w.pitch = 0.0;
    // Same clean-slate reset as a changelevel (the map restarted from scratch).
    w.particles = ParticleSystem::new();
    w.dlights = DynamicLights::new();
    w.trail_org.clear();
    w.beams.clear();
    w.notify_pending.clear();
    w.notify.clear();
    w.centerprint = None;
    w.clock = 0.0;
    w.damage_blend = 0.0;
    w.last_health = f32::NAN;
    w.last_armor = f32::NAN;
    w.oldz = f32::NAN;
    // Same intermission/finale reset as a changelevel (CL_ClearState).
    w.intermission = 0;
    w.completed_time = 0.0;
    w.finale_text.clear();
    w.finale_start = 0.0;
    w.pending_sellscreen = false;
    let _ = w.server.drain_sounds();
    let _ = w.server.drain_particles();
    let _ = w.server.drain_temp_entities();
    let _ = w.server.drain_messages();
    // Stop the dead run's loops; restart the fresh level's (see try_changelevel).
    bump_sound_generation();
    queue_static_sounds(&w.pak, &statics);
    let _ = w.server.drain_svc_events();
}

/// Owned visible-entity descriptor gathered from the server before rendering:
/// `(model name, origin, angles, frame, shirt/pants colour, skin)`.
type EntityDesc = (String, [f32; 3], [f32; 3], usize, [u8; 3], i32);

/// The `backtile` pic (`draw_backtile`, gfx.wad) for [`render::compose_view`],
/// fetched only when the 3-D view leaves part of the screen to tile-clear
/// (viewsize below 120). `None` when the view covers the whole frame or the
/// wad lacks it (then the border fills black).
fn backtile_for(
    vrect: &render::ViewRect,
    render_w: usize,
    render_h: usize,
    gfx_wad: Option<&quake_rs::wad::Wad2>,
) -> Option<Qpic> {
    if vrect.w == render_w && vrect.h == render_h {
        return None;
    }
    gfx_wad.and_then(|g| g.qpic("backtile").ok())
}

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
            Some(Some(mdl)) => Some(Viewmodel { mdl, frame: weapon_frame }),
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
    let refdef = render::calc_refdef(render_w, render_h, w.viewsize, intermission);
    let vrect = refdef.vrect;
    let mut view =
        render::render_scene_ext_sprited(&w.bsp, &cam, vrect.w, vrect.h, &w.palette, &instances, &bmodels, &external, viewmodel, w.clock, &parts, &active_dlights, &light_styles, w.colormap.as_deref(), &sprites);

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
                Some(Viewmodel { mdl, frame: client.weaponframe.max(0) as usize })
            }
            _ => None,
        }
    };
    // SCR_CalcRefdef: the same viewsize framing as live play (the C's demo IS
    // the client rendering a recorded stream).
    let refdef = render::calc_refdef(render_w, render_h, d.viewsize, f.intermission != 0);
    let vrect = refdef.vrect;
    let mut view = render::render_scene_ext_sprited(&d.bsp, &cam, vrect.w, vrect.h, &d.palette, &owned, &bmodels, &[], viewmodel, f.time, &parts, &[], &demo_styles, d.colormap.as_deref(), &sprite_insts);
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
    (img, blend.0, blend.1)
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
    fn queue_sounds_plays_ambience_one_shots() {
        // `ambience/*` is NOT a loop marker on the one-shot path: E1M6's
        // trigger_push wind tunnels fire `sound (other, CHAN_AUTO,
        // "ambience/windfly.wav", 1, ATTN_NORM)` (QuakeC trigger_push_touch)
        // as genuine gameplay one-shots the C plays like any other sample.
        // Only the silent misc/null.wav is skipped.
        let pak = build_test_pak(&[
            ("sound/ambience/windfly.wav", b"WIND"),
            ("sound/misc/null.wav", b"NULL"),
        ]);
        reset_queue();
        queue_sounds(
            &pak,
            &[ev(2, 0, "ambience/windfly.wav", 0.8), ev(3, 0, "misc/null.wav", 0.5)],
            -1,
        );
        let got = drain_queue();
        assert_eq!(got.len(), 1, "windfly queued; null.wav skipped");
        assert_eq!(got[0].0, 0.8, "the ambience one-shot's own params");
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

    // -- dynamic render resolution (set_resolution + clamp + reallocation) ----

    #[test]
    fn clamp_resolution_clamps_into_envelope() {
        // In-range values pass through unchanged.
        assert_eq!(clamp_resolution(640, 400), (640, 400));
        assert_eq!(clamp_resolution(DEFAULT_W as i32, DEFAULT_H as i32), (DEFAULT_W, DEFAULT_H));
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

        // Back to the boot default.
        set_resolution(DEFAULT_W as i32, DEFAULT_H as i32);
        assert_eq!(width(), DEFAULT_W as i32);
        assert_eq!(height(), DEFAULT_H as i32);
    }

    #[test]
    fn boot_then_set_resolution_renders_larger_framebuffer() {
        // Boot the real walk (embedded pak). If the pak is unavailable in this
        // build the test would fail to boot; the workspace embeds a real PAK0.PAK.
        assert_eq!(boot(), 1, "boot the embedded e1m1 walk");
        // Boot keeps the boot default resolution.
        assert_eq!(width(), DEFAULT_W as i32);
        assert_eq!(height(), DEFAULT_H as i32);
        step(0.016);
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            assert_eq!(a.fb.len(), DEFAULT_W * DEFAULT_H * 4, "default fb is DEFAULT_W*DEFAULT_H*4");
        });

        // Pick the largest preset (1280x800, > the 960x600 default), then render:
        // the framebuffer is now 1280*800*4 and the scene rendered into all of it
        // (the fb is fully written by step).
        set_resolution(1280, 800);
        assert_eq!(width(), 1280);
        assert_eq!(height(), 800);
        step(0.016);
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            assert_eq!(a.fb.len(), 1280 * 800 * 4, "step renders into the 1280*800 framebuffer");
            // Every alpha byte is 255 (step pushes opaque RGBA), proving the whole
            // larger buffer was painted, not just the smaller default region.
            assert!(a.fb.chunks_exact(4).all(|px| px[3] == 255), "full fb painted opaque");
        });
    }

    #[test]
    fn menu_left_right_cycle_resolution_and_apply() {
        // A fresh boot sits in the menu on Main. Navigate to Options and cycle the
        // Screen size row with menu_right; the engine's resolution must follow.
        assert_eq!(boot(), 1);
        assert_eq!(menu_visible(), 1, "boot enters the menu");
        // Default render size before touching anything (boot preserves it; a fresh
        // app boots at DEFAULT = 960x600, which is preset index 4).
        assert_eq!(width(), DEFAULT_W as i32);
        assert_eq!(height(), DEFAULT_H as i32);
        // Main cursor 0 is Single Player; move down to Options (item 2) and Enter.
        menu_down(); // -> 1 (Multiplayer)
        menu_down(); // -> 2 (Options)
        menu_select(); // enter Options (cursor on row 0 = Customize controls)
        assert_eq!(menu_visible(), 1);
        // The Screen size row is row 3 (after Customize controls / Go to console /
        // Reset to defaults), so step down 3 rows, then menu_right cycles the preset.
        menu_down(); // -> 1 (Go to console)
        menu_down(); // -> 2 (Reset to defaults)
        menu_down(); // -> 3 (Screen size)
        // menu_right on a non-resolution row wouldn't change the size; on Screen
        // size it cycles to the next preset (960x600 -> 1120x700) and reallocates
        // the framebuffer.
        menu_right();
        assert_eq!((width(), height()), (1120, 700), "right cycles to the 1120x700 preset");
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            assert_eq!(a.fb.len(), 1120 * 700 * 4, "fb reallocated to the new preset");
        });
        // menu_left cycles back to the 960x600 default.
        menu_left();
        assert_eq!((width(), height()), (DEFAULT_W as i32, DEFAULT_H as i32), "left cycles back to the default");
    }

    #[test]
    fn chosen_resolution_persists_across_reboot() {
        // The reported bug: pick a resolution in Options, start the game, and it
        // snapped back to the default. The chosen size must now carry across a
        // re-boot (the 🚶 walk button / New Game), not reset to DEFAULT.
        assert_eq!(boot(), 1);
        // The engine boots at DEFAULT (960x600); pick a different, smaller preset.
        set_resolution(640, 400);
        assert_eq!((width(), height()), (640, 400), "menu/host set the resolution");

        // Re-boot the walk: the resolution MUST be preserved, not reset to DEFAULT.
        assert_eq!(boot(), 1);
        assert_eq!(
            (width(), height()),
            (640, 400),
            "re-boot preserves the chosen resolution (was the reported bug)"
        );
        // ...and the fresh menu's Screen-size preset tracks the live framebuffer,
        // so opening Options shows the real value (no label/fb desync).
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            assert_eq!(a.render_w, 640);
            assert_eq!(a.render_h, 400);
            assert_eq!(
                a.menu.resolution(),
                (640, 400),
                "Options Screen-size label follows the preserved framebuffer"
            );
        });
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

        // `kill` routes through the QuakeC ClientKill (Host_Kill_f), whose
        // respawn() issues localcmd("restart\n") in single player: the level
        // reloads and the player comes back ALIVE with the level-ENTRY loadout
        // — wiping the cheats above and the marker rockets we set here.
        run_console_line("give r 5"); // marker: not part of the entry parms
        assert_eq!(player_field("ammo_rockets"), 5.0);

        // An already-dead player is refused (Host_Kill_f's guard): no QuakeC
        // runs, no restart — the live state is untouched.
        APP.with(|c| {
            let mut b = c.borrow_mut();
            let w = b.as_mut().unwrap().walk.as_mut().unwrap();
            let p = w.player;
            w.server.vm.ent_set_float(p, "health", 0.0);
        });
        run_console_line("kill");
        assert_eq!(
            player_field("ammo_rockets"),
            5.0,
            "a refused kill must not reload the level"
        );
        assert_eq!(player_field("health"), 0.0, "a refused kill leaves the player as-is");

        // Alive again: kill -> ClientKill -> respawn() -> localcmd("restart")
        // -> the level restarts. Fresh player: alive, walking, marker wiped.
        APP.with(|c| {
            let mut b = c.borrow_mut();
            let w = b.as_mut().unwrap().walk.as_mut().unwrap();
            let p = w.player;
            w.server.vm.ent_set_float(p, "health", 70.0);
        });
        run_console_line("kill");
        assert_eq!(player_field("health"), 100.0, "suicide restarted the level: alive");
        assert_eq!(player_field("deadflag"), 0.0, "fresh player is not dead");
        assert_eq!(
            player_field("movetype"),
            MOVETYPE_WALK,
            "fresh player walks (the fly cheat did not survive the restart)"
        );
        assert_eq!(
            player_field("ammo_rockets"),
            0.0,
            "restart restored the level-ENTRY parms (the marker is gone)"
        );
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

    /// QuakeC deadflag values (client.qc / defs.qc).
    const DEAD_DYING: f32 = 1.0;
    const DEAD_DEAD: f32 = 2.0;
    const DEAD_RESPAWNABLE: f32 = 3.0;
    /// IT_ROCKET_LAUNCHER (defs.qc).
    const IT_RL: i32 = 32;

    /// End-to-end proof of the single-player death -> respawn chain on the REAL
    /// embedded e1m1 + progs.dat, through the exact path the browser uses
    /// (`boot()` / `step()`):
    ///
    ///   self-fired rocket -> T_RadiusDamage -> T_Damage -> Killed -> PlayerDie
    ///   (deadflag = DEAD_DYING, movetype = TOSS: corpse physics) -> the
    ///   death-anim THINKS play out while health < 0 (PlayerDead -> deadflag =
    ///   DEAD_DEAD) -> PlayerDeathThink (run from PlayerPreThink while dead)
    ///   sees all buttons released (deadflag = DEAD_RESPAWNABLE) -> a +attack
    ///   press reaches the QuakeC `button0` field while dead -> respawn() ->
    ///   localcmd("restart\n") -> take_pending_restart -> try_restart reloads
    ///   the level with the level-ENTRY parms: the player is alive at the spawn
    ///   point in a reset world.
    #[test]
    fn real_death_chain_respawns_via_restart() {
        assert_eq!(boot(), 1, "boot builds a walk from the embedded pak");
        set_resolution(320, 200); // keep the per-step debug render cheap
        // boot() opens the main menu, which gates gameplay input; close it.
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);

        // Arm the rocket launcher and aim straight down (test SETUP only — the
        // kill itself travels the real QuakeC damage chain). Health 30: one
        // self-rocket deals ~55 (radius 120 minus distance falloff, halved for
        // attacker == target), leaving ~-25 — dead, but above the -40 gib line,
        // so the longer death-ANIM think chain runs.
        let spawn_org = APP.with(|c| {
            let mut b = c.borrow_mut();
            let w = b.as_mut().unwrap().walk.as_mut().unwrap();
            let p = w.player;
            w.pitch = 80.0; // straight down (the +80 clamp)
            w.server.vm.ent_set_float(p, "health", 30.0);
            let items = w.server.vm.ent_get_float(p, "items") as i32 | IT_RL;
            w.server.vm.ent_set_float(p, "items", items as f32);
            w.server.vm.ent_set_float(p, "ammo_rockets", 5.0);
            player_start(&w.bsp.entities).expect("e1m1 has info_player_start").0
        });

        // Select the RL through the REAL impulse path (PlayerPostThink ->
        // W_WeaponFrame -> ImpulseCommands -> W_ChangeWeapon -> W_SetCurrentAmmo).
        APP.with(|c| {
            c.borrow_mut().as_mut().unwrap().walk.as_mut().unwrap().next_impulse = 7
        });
        step(0.05);
        assert_eq!(
            player_field("weapon") as i32,
            IT_RL,
            "impulse 7 selected the rocket launcher"
        );

        // Settle on the floor, then FIRE for one frame and release.
        for _ in 0..4 {
            step(0.05);
        }
        let mut trace: Vec<(usize, f32, f32)> = Vec::new(); // (frame, health, deadflag)
        trace.push((0, player_field("health"), player_field("deadflag")));
        set_attack(1);
        step(0.05);
        set_attack(0);

        // Ride the death out with all buttons released, tracing deadflag per
        // frame. Once DYING, hold +attack for ONE frame to prove UserCmd buttons
        // reach the QuakeC button0 field while dead (nothing consumes it during
        // DEAD_DYING: PlayerPreThink returns early and W_WeaponFrame is
        // deadflag-gated), then release well before the DEAD_DEAD button-free
        // wait.
        let mut probed_button_while_dead = false;
        for i in 1..=120 {
            step(0.05);
            let (h, df) = (player_field("health"), player_field("deadflag"));
            trace.push((i, h, df));
            if df == DEAD_DYING && !probed_button_while_dead {
                set_attack(1);
                step(0.05);
                assert_eq!(
                    player_field("button0"),
                    1.0,
                    "the attack button reaches QuakeC button0 while dead"
                );
                assert!(player_field("health") < 0.0, "the probe ran while dead");
                set_attack(0);
                step(0.05); // settle the release
                probed_button_while_dead = true;
            }
            if df == DEAD_RESPAWNABLE {
                break;
            }
        }
        assert!(probed_button_while_dead, "the DEAD_DYING phase was observed");

        // The chain, in order: alive -> DYING (PlayerDie, via the real
        // T_Damage) -> DEAD (the death-anim thinks ran out while health < 0 —
        // client thinks RUN while dead) -> RESPAWNABLE (PlayerDeathThink, run
        // from PlayerPreThink while dead, saw every button released).
        let mut seq: Vec<f32> = Vec::new();
        for &(i, h, df) in &trace {
            if seq.last() != Some(&df) {
                seq.push(df);
                // Evidence of the chain as it executed (visible with --nocapture).
                eprintln!("deadflag -> {df} at frame {i} (health {h})");
            }
        }
        assert_eq!(
            seq,
            vec![0.0, DEAD_DYING, DEAD_DEAD, DEAD_RESPAWNABLE],
            "deadflag progression; full trace: {trace:?}"
        );

        // DEAD_RESPAWNABLE: the dead player waits for a button. Press +attack:
        // PlayerDeathThink consumes it and calls respawn() ->
        // localcmd("restart\n"); step_walk drains take_pending_restart() and
        // try_restart() reloads the level inside this same step.
        let t_before = APP.with(|c| {
            c.borrow().as_ref().unwrap().walk.as_ref().unwrap().server.time()
        });
        set_attack(1);
        step(0.05);
        set_attack(0);

        assert_eq!(player_field("health"), 100.0, "respawned alive (entry health)");
        assert_eq!(player_field("deadflag"), 0.0, "fresh player is not dead");
        assert_eq!(player_field("movetype"), MOVETYPE_WALK, "fresh player walks");
        let items = player_field("items") as i32;
        assert_eq!(
            items & IT_RL,
            0,
            "the cheat rocket launcher did NOT survive (level-ENTRY parms restored)"
        );
        assert_ne!(items & IT_SHOTGUN, 0, "the entry loadout (shotgun) is back");
        assert_eq!(player_field("ammo_rockets"), 0.0, "cheat rockets wiped");
        assert_eq!(player_field("ammo_shells"), 25.0, "entry shells restored");

        // Back at the spawn point, in a rebuilt world (server time restarted).
        let (org, t_after) = APP.with(|c| {
            let b = c.borrow();
            let w = b.as_ref().unwrap().walk.as_ref().unwrap();
            (w.server.vm.ent_get_vector(w.player, "origin"), w.server.time())
        });
        assert!(
            (org[0] - spawn_org[0]).abs() < 16.0
                && (org[1] - spawn_org[1]).abs() < 16.0
                && (org[2] - spawn_org[2]).abs() < 64.0,
            "respawned at the spawn point: {org:?} vs {spawn_org:?}"
        );
        assert!(
            t_after < t_before,
            "the world was rebuilt: server time restarted ({t_after} < {t_before})"
        );
    }

    /// An ENVIRONMENT kill reaches the same chain: slime damage is dealt by
    /// client.qc `WaterMove` (run from PlayerPreThink) -> `T_Damage(self, world,
    /// world, 4*waterlevel)` -> Killed -> PlayerDie — the attacker==world branch
    /// (no knockback), unlike the rocket. Teleporting the player into e1m1's
    /// slime pool is test setup; the damage itself travels the real QuakeC path,
    /// and the death rides the same anim -> DEAD_RESPAWNABLE -> button ->
    /// restart tail.
    #[test]
    fn environment_slime_kill_enters_the_same_death_chain() {
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);

        // Find a submerged spot: scan the world bounds on a coarse grid for
        // CONTENTS_SLIME that is still slime 48 units higher, so a player with
        // origin 24 above the probe has the eye (origin + 22) under the surface
        // (waterlevel 3 -> 12 damage per slime tick).
        let slime: Option<[f32; 3]> = APP.with(|c| {
            let b = c.borrow();
            let w = b.as_ref().unwrap().walk.as_ref().unwrap();
            let world = &w.bsp.models[0];
            let (mins, maxs) = (world.mins, world.maxs);
            let mut z = mins[2] + 16.0;
            while z < maxs[2] {
                let mut x = mins[0] + 16.0;
                while x < maxs[0] {
                    let mut y = mins[1] + 16.0;
                    while y < maxs[1] {
                        if quake_rs::world::point_contents(&w.bsp, [x, y, z])
                            == quake_rs::bsp::CONTENTS_SLIME
                            && quake_rs::world::point_contents(&w.bsp, [x, y, z + 48.0])
                                == quake_rs::bsp::CONTENTS_SLIME
                        {
                            return Some([x, y, z]);
                        }
                        y += 64.0;
                    }
                    x += 64.0;
                }
                z += 64.0;
            }
            None
        });
        let p = slime.expect("e1m1 has a slime pool deep enough to submerge in");

        // Drop the player in with 5 health: the first WaterMove slime tick
        // (4 * waterlevel) kills through the real chain. waterlevel is sensed
        // during the move phase, so the kill lands a couple of frames in.
        APP.with(|c| {
            let mut b = c.borrow_mut();
            let w = b.as_mut().unwrap().walk.as_mut().unwrap();
            let pl = w.player;
            w.server.vm.ent_set_vector(pl, "origin", [p[0], p[1], p[2] + 24.0]);
            w.server.vm.ent_set_vector(pl, "velocity", [0.0, 0.0, 0.0]);
            w.server.vm.ent_set_float(pl, "health", 5.0);
        });
        let mut died = false;
        for _ in 0..20 {
            step(0.05);
            if player_field("deadflag") >= DEAD_DYING {
                died = true;
                break;
            }
        }
        assert!(died, "slime damage killed through PlayerDie (deadflag set)");
        assert!(player_field("health") < 0.0, "the slime tick took health below zero");

        // Same tail as the rocket death: anim out, button, restart, alive.
        let mut respawnable = false;
        for _ in 0..120 {
            step(0.05);
            if player_field("deadflag") == DEAD_RESPAWNABLE {
                respawnable = true;
                break;
            }
        }
        assert!(respawnable, "the death anim ran out to DEAD_RESPAWNABLE");
        set_attack(1);
        step(0.05);
        set_attack(0);
        assert_eq!(
            player_field("health"),
            100.0,
            "respawned alive after the environment kill"
        );
        assert_eq!(player_field("deadflag"), 0.0);
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

    // ------------------------------------------------ ambient sounds (H11)

    /// A minimal PCM mono 11025 Hz 8-bit WAV; `loop_start = Some(n)` adds the
    /// `cue ` chunk the ambience samples carry (the loop gate `S_StaticSound`
    /// checks before agreeing to static-loop a sound).
    fn test_wav(loop_start: Option<u32>, data_samples: u32) -> Vec<u8> {
        let mut b: Vec<u8> = Vec::new();
        b.extend(b"RIFF");
        b.extend(0u32.to_le_bytes());
        b.extend(b"WAVE");
        b.extend(b"fmt ");
        b.extend(16u32.to_le_bytes());
        b.extend(1u16.to_le_bytes()); // PCM
        b.extend(1u16.to_le_bytes()); // mono
        b.extend(11025u32.to_le_bytes()); // rate
        b.extend(11025u32.to_le_bytes()); // byte rate
        b.extend(1u16.to_le_bytes()); // block align
        b.extend(8u16.to_le_bytes()); // bits
        if let Some(ls) = loop_start {
            b.extend(b"cue ");
            b.extend(28u32.to_le_bytes());
            b.extend(1u32.to_le_bytes()); // one cue point
            b.extend([0u8; 16]); // id/position/chunkid/chunkstart
            b.extend(0u32.to_le_bytes()); // block start
            b.extend(ls.to_le_bytes()); // sample offset = loop start
        }
        b.extend(b"data");
        b.extend(data_samples.to_le_bytes());
        b.extend(std::iter::repeat_n(0x80u8, data_samples as usize));
        b
    }

    /// Spawn a real map's entities on a live server and return the static
    /// sounds its QuakeC registered (ambientsound() during spawn).
    fn spawn_map_statics(map: &str) -> Vec<StaticSound> {
        let pak = pak().expect("embedded pak");
        let read = |n: &str| pak.read_file(n).ok().flatten();
        let bsp = Bsp::parse(&read(map).expect("bsp")).expect("parse");
        let progs = Progs::parse(&read("progs.dat").expect("progs")).expect("parse");
        let mut server = Server::with_pak(bsp, progs, Some(pak.clone())).expect("server");
        let _ = server.drain_static_sounds();
        server.spawn_entities().expect("spawn");
        let statics = server.drain_static_sounds();
        // The registry drains once: a second drain is empty.
        assert!(server.drain_static_sounds().is_empty());
        statics
    }

    #[test]
    fn e1m2_spawn_registers_fire_torch_static_sounds() {
        // Ground truth: the medieval maps' wall torches run QuakeC's
        // FireAmbient — `ambientsound(self.origin, "ambience/fire1.wav", 0.5,
        // ATTN_STATIC)` — during spawn. e1m2 places 24 of them.
        let statics = spawn_map_statics("maps/e1m2.bsp");
        let fires: Vec<&StaticSound> = statics
            .iter()
            .filter(|s| s.sample == "ambience/fire1.wav")
            .collect();
        assert!(
            fires.len() >= 20,
            "e1m2's torches register ambience/fire1.wav loops (got {})",
            fires.len()
        );
        for f in &fires {
            // FireAmbient's exact arguments through the wire bytes:
            // vol 0.5 -> 127/255, ATTN_STATIC 3 -> 192/64 = 3.0.
            assert_eq!(f.volume, 127.0 / 255.0, "torch volume 0.5 (quantized)");
            assert_eq!(f.attenuation, 3.0, "ATTN_STATIC");
            assert!(
                f.origin.iter().all(|c| c.abs() < 10000.0),
                "plausible world position {:?}",
                f.origin
            );
            assert!(f.sound_index >= 1, "fire1.wav resolved to a precache slot");
        }
        // The torches sit at DISTINCT places (each wall torch registers its own).
        let mut pts: Vec<[i32; 3]> = fires
            .iter()
            .map(|f| [f.origin[0] as i32, f.origin[1] as i32, f.origin[2] as i32])
            .collect();
        pts.sort_unstable();
        pts.dedup();
        assert!(
            pts.len() >= 20,
            "torch loops are at distinct world positions (got {})",
            pts.len()
        );
    }

    #[test]
    fn e1m1_spawn_registers_base_ambience_static_sounds() {
        // e1m1 is a BASE map: no torches, but its light_fluoro fixtures hum
        // (ambience/fl_hum1.wav) and its computers drone (ambience/comp1.wav),
        // all at ATTN_STATIC — the level's actual placed soundscape.
        let statics = spawn_map_statics("maps/e1m1.bsp");
        assert!(
            statics.iter().any(|s| s.sample == "ambience/fl_hum1.wav"),
            "fluorescent hum registered"
        );
        assert!(
            statics.iter().any(|s| s.sample == "ambience/comp1.wav"),
            "computer drone registered"
        );
        assert!(statics.len() >= 10, "a full soundscape (got {})", statics.len());
        for s in &statics {
            assert_eq!(s.attenuation, 3.0, "every e1m1 ambient is ATTN_STATIC");
            assert!(s.volume > 0.0 && s.volume <= 1.0);
        }
    }

    #[test]
    fn e1m1_leafs_carry_ambient_levels() {
        // The dleaf_t ambient_level[NUM_AMBIENTS] bytes must survive the real
        // map's parse: e1m1 has both water (slime pools) and open-sky areas, so
        // SOME leafs carry non-zero water and sky levels for
        // S_UpdateAmbientSounds to ramp toward.
        let pak = pak().expect("embedded pak");
        let bsp = Bsp::parse(
            &pak.read_file("maps/e1m1.bsp").ok().flatten().expect("bsp"),
        )
        .expect("parse");
        let water = bsp
            .leafs
            .iter()
            .filter(|l| l.ambient_level[quake_rs::snd::AMBIENT_WATER] > 0)
            .count();
        let sky = bsp
            .leafs
            .iter()
            .filter(|l| l.ambient_level[quake_rs::snd::AMBIENT_SKY] > 0)
            .count();
        assert!(water > 0, "some e1m1 leafs hear water ambience");
        assert!(sky > 0, "some e1m1 leafs hear sky/wind ambience");
    }

    #[test]
    fn boot_walk_queues_static_loops_and_demo_boot_bumps_generation() {
        // boot() (live e1m1) must hand the page the level's static loops via
        // poll_static_sound, each with sane spatial params + loop window; then
        // boot_demo() must bump the generation (the page's stop-all signal) and
        // replace the queue with the DEMO's own signon statics.
        assert_eq!(boot(), 1);
        let g0 = sound_generation();
        let mut n = 0;
        loop {
            let len = poll_static_sound();
            if len == 0 {
                break;
            }
            n += 1;
            let (ls, le) = (sound_loop_start(), sound_loop_end());
            assert!(ls >= 0.0, "loop start is a real offset");
            assert!(le > ls, "loop end past loop start");
            assert!(sound_volume() > 0.0 && sound_volume() <= 1.0);
            assert!(sound_attenuation() > 0.0 && sound_attenuation() <= 4.0);
            assert_eq!(sound_is_view_entity(), 0, "statics are world-placed");
        }
        assert!(n >= 5, "e1m1 queues its torch/ambience loops (got {n})");

        // Switching to the attract demo stops the walk's loops (generation
        // bump) and registers the demo signon's own statics.
        assert_eq!(boot_demo(), 1);
        assert_ne!(sound_generation(), g0, "mode transition bumps generation");
        assert!(
            poll_static_sound() > 0,
            "the demo's svc_spawnstaticsound loops are queued"
        );
    }

    #[test]
    fn load_ambient_sound_loads_only_water_and_wind() {
        // S_Init loads exactly ambience/water1.wav (ch 0) + ambience/wind2.wav
        // (ch 1); slime/lava keep a NULL sfx. Both real samples carry cue loop
        // chunks, so the loop window must come back usable.
        let l0 = load_ambient_sound(0);
        assert!(l0 > 0, "water1.wav loaded from the pak");
        assert!(sound_loop_end() > 0.0, "water1 loop window parsed");
        let l1 = load_ambient_sound(1);
        assert!(l1 > 0, "wind2.wav loaded from the pak");
        assert!(sound_loop_end() > 0.0, "wind2 loop window parsed");
        assert_eq!(load_ambient_sound(2), 0, "slime: WinQuake never loads one");
        assert_eq!(load_ambient_sound(3), 0, "lava: WinQuake never loads one");
        assert_eq!(load_ambient_sound(4), 0, "out of range");
        assert_eq!(load_ambient_sound(-1), 0, "negative channel");
    }

    #[test]
    fn ambient_gain_serves_frame_volumes_and_resets_on_generation_bump() {
        AMBIENT.with(|a| *a.borrow_mut() = AmbientChannels::new());
        // 36 host frames (1/72 s each) toward a full-water leaf: the C's
        // integer ramp climbs +1 per step -> master_vol 36.
        for _ in 0..36 {
            ramp_ambient_channels(Some(&[255, 0, 0, 0]), 1.0 / 72.0);
        }
        assert!((ambient_gain(0) - 36.0 / 255.0).abs() < 1e-6, "ramped gain");
        assert_eq!(ambient_gain(1), 0.0, "silent channel");
        assert_eq!(ambient_gain(99), 0.0, "out of range");
        assert_eq!(ambient_gain(-1), 0.0, "negative channel");
        // Outside the world (the C's `!l` branch): this frame is SILENT on the
        // page even though master_vol is preserved — ambient_gain must serve
        // update()'s returned frame volumes, not the raw ramp state.
        ramp_ambient_channels(None, 1.0 / 72.0);
        assert_eq!(ambient_gain(0), 0.0, "no leaf = silence NOW");
        // Re-entering the world resumes the ramp from the preserved value.
        ramp_ambient_channels(Some(&[255, 0, 0, 0]), 1.0 / 72.0);
        assert!((ambient_gain(0) - 37.0 / 255.0).abs() < 1e-6, "ramp resumed");
        // A level change (S_StopAllSounds) resets ramp AND frame volumes.
        bump_sound_generation();
        assert_eq!(ambient_gain(0), 0.0, "generation bump resets the ramp");
    }

    #[test]
    fn queue_static_sounds_drops_unlooped_and_missing_samples() {
        // S_StaticSound's gates: a sample with no cue loop point is REFUSED
        // ("Sound %s not looped"), a missing file is dropped, and names resolve
        // under the pak's "sound/" directory. Only the looped one queues.
        let looped = test_wav(Some(8), 64);
        let oneshot = test_wav(None, 64);
        let pak = build_test_pak(&[
            ("sound/amb/loopy.wav", looped.as_slice()),
            ("sound/amb/shot.wav", oneshot.as_slice()),
        ]);
        STATIC_QUEUE.with(|q| q.borrow_mut().clear());
        let mk = |sample: &str| StaticSound {
            origin: [1.0, 2.0, 3.0],
            sound_index: 1,
            sample: sample.to_string(),
            volume: 0.5,
            attenuation: 3.0,
        };
        queue_static_sounds(
            &pak,
            &[mk("amb/loopy.wav"), mk("amb/shot.wav"), mk("amb/missing.wav"), mk("")],
        );
        let len = poll_static_sound();
        assert_eq!(len, looped.len() as i32, "the looped sample queued");
        assert_eq!(sound_origin_x(), 1.0);
        assert_eq!(sound_origin_y(), 2.0);
        assert_eq!(sound_origin_z(), 3.0);
        assert_eq!(sound_volume(), 0.5);
        assert_eq!(sound_attenuation(), 3.0);
        // Loop window in seconds: start 8/11025, end 64/11025.
        assert!((sound_loop_start() - 8.0 / 11025.0).abs() < 1e-7);
        assert!((sound_loop_end() - 64.0 / 11025.0).abs() < 1e-7);
        assert_eq!(
            poll_static_sound(),
            0,
            "unlooped / missing / empty-name samples all dropped"
        );
    }

    #[test]
    fn queue_static_sounds_cap_and_slot_burning_match_the_c() {
        // S_StaticSound's budget is 116 (MAX_CHANNELS 128 minus the 12
        // channels total_channels starts at), and `total_channels++` happens
        // BEFORE the load/loop checks — a registration that then fails burns
        // its slot. The C's `if (!sfx) return` (empty name) precedes the slot
        // grab and burns nothing.
        let looped = test_wav(Some(8), 64);
        let oneshot = test_wav(None, 64);
        let pak = build_test_pak(&[
            ("sound/amb/loopy.wav", looped.as_slice()),
            ("sound/amb/shot.wav", oneshot.as_slice()),
        ]);
        let mk = |sample: &str| StaticSound {
            origin: [0.0; 3],
            sound_index: 1,
            sample: sample.to_string(),
            volume: 0.5,
            attenuation: 3.0,
        };
        // 3 empty names (no slot), 4 unlooped (slot burned, dropped), then 116
        // looped: only 112 slots remain for them.
        let mut statics = vec![mk(""), mk(""), mk("")];
        statics.extend(std::iter::repeat_with(|| mk("amb/shot.wav")).take(4));
        statics.extend(std::iter::repeat_with(|| mk("amb/loopy.wav")).take(116));
        STATIC_QUEUE.with(|q| q.borrow_mut().clear());
        queue_static_sounds(&pak, &statics);
        let mut queued = 0;
        while poll_static_sound() > 0 {
            queued += 1;
        }
        assert_eq!(
            queued,
            MAX_STATIC_SOUNDS - 4,
            "4 burned slots leave 112 of the 116 for real loops"
        );
    }

    // -------------------------------------------------------------------
    // Intermission + finale (end-to-end against the real progs.dat)
    // -------------------------------------------------------------------

    /// Borrow the live walk mutably (panics if no walk — these tests boot first).
    fn walk_mut<R>(f: impl FnOnce(&mut Walk) -> R) -> R {
        APP.with(|c| f(c.borrow_mut().as_mut().unwrap().walk.as_mut().unwrap()))
    }

    /// Close the App-level menu: `boot()` opens it over the walk, and while it is
    /// up (`key_dest != key_game`) the gameplay buttons IntermissionThink polls
    /// are gated to 0 — the player must dismiss it, and so must these tests.
    fn close_menu() {
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.close());
    }

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

    // ------------------------------------------------------------ save/load

    /// The whole console scrollback as one string (oldest line first).
    fn console_text() -> String {
        APP.with(|c| {
            c.borrow()
                .as_ref()
                .map(|a| a.console.lines().collect::<Vec<_>>().join("\n"))
                .unwrap_or_default()
        })
    }

    /// A deterministic world-state digest: the player's view/inventory fields,
    /// the QC counters, and — for EVERY edict slot — classname, origin,
    /// velocity, solidity and the think schedule (function NAME + nextthink).
    /// Floats are formatted at the save format's own `%.6f` precision, so an
    /// exact text round-trip digests equal while any real divergence
    /// (a door mid-move, a monster's next think) shows up.
    fn world_digest() -> String {
        use std::fmt::Write as _;
        APP.with(|c| {
            let b = c.borrow();
            let w = b.as_ref().expect("app").walk.as_ref().expect("walk");
            let vm = &w.server.vm;
            let f6 = |v: f32| format!("{v:.6}");
            let v6 = |v: [f32; 3]| format!("{:.6} {:.6} {:.6}", v[0], v[1], v[2]);
            let mut d = String::new();
            let p = w.player;
            let _ = writeln!(
                d,
                "player org={} vel={} ang={} vang={}",
                v6(vm.ent_get_vector(p, "origin")),
                v6(vm.ent_get_vector(p, "velocity")),
                v6(vm.ent_get_vector(p, "angles")),
                v6(vm.ent_get_vector(p, "v_angle")),
            );
            for name in [
                "health", "armorvalue", "items", "weapon", "currentammo",
                "ammo_shells", "ammo_nails", "ammo_rockets", "ammo_cells",
                "deadflag", "weaponframe", "frags",
            ] {
                let _ = writeln!(d, "p.{name}={}", f6(vm.ent_get_float(p, name)));
            }
            let _ = writeln!(
                d,
                "time={} skill={} serverflags={} killed={} secrets={}",
                f6(w.server.time()),
                w.server.skill(),
                f6(vm.gget_float("serverflags")),
                f6(vm.gget_float("killed_monsters")),
                f6(vm.gget_float("found_secrets")),
            );
            for e in 0..vm.num_edicts() {
                let ent = e as i32;
                if vm.is_free_edict(ent) {
                    let _ = writeln!(d, "{e}: free");
                    continue;
                }
                let think = vm.ent_get_int(ent, "think");
                let think_name = vm
                    .progs
                    .functions
                    .get(think as usize)
                    .map(|f| vm.progs.string(f.s_name).to_string())
                    .unwrap_or_default();
                let _ = writeln!(
                    d,
                    "{e}: {} org={} vel={} solid={} nextthink={} think={} frame={} health={}",
                    vm.ent_string_ref(ent, "classname"),
                    v6(vm.ent_get_vector(ent, "origin")),
                    v6(vm.ent_get_vector(ent, "velocity")),
                    f6(vm.ent_get_float(ent, "solid")),
                    f6(vm.ent_get_float(ent, "nextthink")),
                    think_name,
                    f6(vm.ent_get_float(ent, "frame")),
                    f6(vm.ent_get_float(ent, "health")),
                );
            }
            d
        })
    }

    /// `Host_Savegame_f`/`Host_Loadgame_f` console guards, with the C's exact
    /// messages: demo mode refuses ("Not playing a local game."), intermission
    /// refuses, bad argc prints usage, ".." is rejected, a dead player refuses,
    /// and a missing stored slot reports the C's fopen failure.
    #[test]
    fn save_console_guards_match_host_savegame_f() {
        // Attract mode (demo playing) = !sv.active for this shell.
        assert_eq!(boot_attract(), 1);
        let in_demo = APP.with(|c| c.borrow().as_ref().unwrap().mode == 1);
        assert!(in_demo, "boot_attract plays the demo");
        console_toggle();
        run_console_line("save nope");
        assert!(
            console_text().contains("Not playing a local game."),
            "demo mode refuses: {}",
            console_text()
        );

        // A live walk now (console stays open across boot()).
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);

        run_console_line("save");
        assert!(console_text().contains("save <savename> : save a game"));
        run_console_line("save ../evil");
        assert!(console_text().contains("Relative pathnames are not allowed."));

        walk_mut(|w| w.intermission = 1);
        run_console_line("save x");
        assert!(console_text().contains("Can't save in intermission."));
        walk_mut(|w| w.intermission = 0);

        let p = walk_mut(|w| w.player);
        walk_mut(|w| w.server.vm.ent_set_float(p, "health", 0.0));
        run_console_line("save x");
        assert!(console_text().contains("Can't savegame with a dead player"));
        walk_mut(|w| w.server.vm.ent_set_float(p, "health", 100.0));

        run_console_line("load");
        assert!(console_text().contains("load <savename> : load a game"));

        // load of a slot the page can't find -> the page calls load_failed().
        run_console_line("load missing_slot");
        assert!(console_text().contains("Loading game from missing_slot.sav..."));
        assert!(poll_load_request() > 0, "the request reaches the page");
        let name = LOAD_REQ_CUR.with(|c| c.borrow().clone());
        assert_eq!(name, "missing_slot.sav");
        load_failed();
        assert!(console_text().contains("ERROR: couldn't open."));

        // And a healthy save passes the guards and queues for the page.
        run_console_line("save ok_slot");
        let text = console_text();
        assert!(text.contains("Saving game to ok_slot.sav..."), "{text}");
        assert!(text.contains("done."), "{text}");
        assert!(poll_save() > 0, "the .sav text is queued for the page");
        let (fname, sav) = SAVE_CUR.with(|c| c.borrow().clone());
        assert_eq!(fname, "ok_slot.sav");
        assert!(sav.starts_with("5\n"), "SAVEGAME_VERSION header");
    }

    /// End-to-end round-trip on the real embedded e1m1 + progs.dat through the
    /// exact browser path: play (walk toward the first door, self-rocket for
    /// REAL damage), `save` via the console, keep playing (divergence), then
    /// feed the stored text back like the page does and `load` — the world
    /// digest (player + every edict's origin/think schedule + counters) must
    /// equal the saved instant, and the loaded game must keep running.
    #[test]
    fn save_load_round_trips_the_world_digest() {
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);

        // Arm the rocket launcher (setup only — the damage itself travels the
        // real QuakeC chain) and select it through the real impulse path.
        walk_mut(|w| {
            let p = w.player;
            let items = w.server.vm.ent_get_float(p, "items") as i32 | IT_RL;
            w.server.vm.ent_set_float(p, "items", items as f32);
            w.server.vm.ent_set_float(p, "ammo_rockets", 5.0);
            w.next_impulse = 7;
        });
        step(0.05);

        // ~2.5s forward: across e1m1's start walkway toward the first door
        // (its trigger opens it — moving brush state for the digest).
        set_move(1.0, 0.0);
        for _ in 0..50 {
            step(0.05);
        }
        set_move(0.0, 0.0);

        // One self-rocket at the floor: T_RadiusDamage drops real health.
        walk_mut(|w| w.pitch = 80.0);
        set_attack(1);
        step(0.05);
        set_attack(0);
        for _ in 0..20 {
            step(0.05); // the rocket resolves; the world settles
        }
        walk_mut(|w| w.pitch = 0.0);
        step(0.05);
        let health = player_field("health");
        assert!(
            health > 0.0 && health < 100.0,
            "took real (survivable) rocket damage: {health}"
        );

        let digest_saved = world_digest();

        // Save through the REAL console path; grab what the page would store.
        console_toggle();
        run_console_line("save sl_round");
        let len = poll_save();
        assert!(len > 0, "a completed save is queued");
        let (fname, text) = SAVE_CUR.with(|c| c.borrow().clone());
        assert_eq!(fname, "sl_round.sav");
        assert_eq!(len as usize, text.len());
        console_toggle();

        // Keep playing: the world diverges from the saved instant.
        for _ in 0..40 {
            step(0.05);
        }
        assert_ne!(world_digest(), digest_saved, "play diverged after saving");

        // Load: the console requests, the page feeds the text back.
        console_toggle();
        run_console_line("load sl_round");
        assert!(poll_load_request() > 0);
        SAV_BUF.with(|b| *b.borrow_mut() = text.clone().into_bytes());
        assert_eq!(load_game(), 1, "the stored save loads");

        // ROUND-TRIP FIDELITY: the reloaded world equals the saved instant.
        assert_eq!(world_digest(), digest_saved, "load restored the saved world");

        // 100 frames crash-free on the loaded world.
        for _ in 0..100 {
            step(0.05);
        }
        assert!(player_field("health") > 0.0, "the loaded game keeps playing");
    }

    /// The page-reload path: save, tear the whole App down (a fresh browser
    /// session), boot, and load the stored text — the digest still matches.
    #[test]
    fn fresh_boot_then_load_restores_the_saved_digest() {
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);
        set_move(1.0, 0.0);
        for _ in 0..30 {
            step(0.05);
        }
        set_move(0.0, 0.0);
        step(0.05);

        let digest_saved = world_digest();
        console_toggle();
        run_console_line("save sl_reload");
        assert!(poll_save() > 0);
        let (_, text) = SAVE_CUR.with(|c| c.borrow().clone());

        // "Page reload": drop the entire App and boot a fresh session.
        APP.with(|c| *c.borrow_mut() = None);
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);

        SAV_BUF.with(|b| *b.borrow_mut() = text.into_bytes());
        assert_eq!(load_game(), 1, "the save loads in the fresh session");
        assert_eq!(
            world_digest(),
            digest_saved,
            "a fresh boot + load restores the same world"
        );
        for _ in 0..50 {
            step(0.05);
        }
        assert!(player_field("health") > 0.0);
    }

    /// Hostile input: garbage text, a wrong version, and a truncated save all
    /// fail CLEANLY — error on the console, the running game untouched, no
    /// panic. (The C `Sys_Error`s; degrading is the documented deviation.)
    #[test]
    fn hostile_sav_text_fails_cleanly_and_keeps_the_game() {
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);
        for _ in 0..5 {
            step(0.05);
        }
        let digest = world_digest();

        // Total garbage (including non-UTF8 bytes).
        SAV_BUF.with(|b| *b.borrow_mut() = b"complete {{{ garbage \x01\xff".to_vec());
        assert_eq!(load_game(), 0, "garbage is rejected");
        assert_eq!(world_digest(), digest, "the running game is untouched");

        // A real save to corrupt.
        console_toggle();
        run_console_line("save sl_hostile");
        assert!(poll_save() > 0);
        let (_, text) = SAVE_CUR.with(|c| c.borrow().clone());
        console_toggle();

        // Wrong version: the C's exact message.
        let mut wrong = text.clone();
        wrong.replace_range(0..1, "9");
        SAV_BUF.with(|b| *b.borrow_mut() = wrong.into_bytes());
        assert_eq!(load_game(), 0);
        assert!(
            console_text().contains("Savegame is version 9, not 5"),
            "{}",
            console_text()
        );
        assert_eq!(world_digest(), digest);

        // Truncated mid-block (cut inside the last "classname" key).
        let cut = text.rfind("\"classname\"").expect("save has classnames") + 5;
        SAV_BUF.with(|b| *b.borrow_mut() = text.as_bytes()[..cut].to_vec());
        assert_eq!(load_game(), 0, "a truncated save is rejected");
        assert_eq!(world_digest(), digest, "still untouched");

        // A rejected save must not leak its HEADER into the shared per-thread
        // transports the surviving game syncs from each frame: doctor the
        // header to a hostile skill (line 18) and style-0 pattern (line 21),
        // truncate the blocks so the load fails AFTER those were applied, and
        // confirm the running game's skill/lightstyle survive the next frame
        // (review finding: the failed-load restore in load_savegame).
        let style0 = walk_mut(|w| w.server.lightstyle(0).to_string());
        let skill = walk_mut(|w| w.server.skill());
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        lines[18] = "3".into(); // hostile current_skill
        lines[21] = "hostilepattern".into(); // hostile lightstyle 0
        let doctored = lines.join("\n");
        let cut = doctored.rfind("\"classname\"").expect("blocks survive doctoring") + 5;
        SAV_BUF.with(|b| *b.borrow_mut() = doctored.as_bytes()[..cut].to_vec());
        assert_eq!(load_game(), 0, "the doctored save is still rejected");
        assert_eq!(world_digest(), digest, "world (incl. skill) untouched");
        step(0.05); // run_frame re-syncs lightstyles from the shared transport
        assert_eq!(
            walk_mut(|w| w.server.lightstyle(0).to_string()),
            style0,
            "the failed load's lightstyles must not bleed into the survivor"
        );
        assert_eq!(walk_mut(|w| w.server.skill()), skill, "skill restored");

        // And the (never-replaced) game keeps stepping fine.
        for _ in 0..20 {
            step(0.05);
        }
        assert!(player_field("health") > 0.0);
    }

    /// `sav_alloc` fails CLOSED on a hostile length: NULL back (the page then
    /// skips the copy and reports `load_failed`) — never a pointer that
    /// invites a `len`-byte write the engine did not allocate (review
    /// finding: the old clamp-to-empty returned a dangling pointer the page
    /// would copy a >8 MB localStorage value through, smashing linear memory).
    #[test]
    fn sav_alloc_rejects_hostile_lengths_with_null() {
        assert!(sav_alloc(SAV_BUF_MAX + 1).is_null());
        assert!(sav_alloc(i32::MAX).is_null());
        assert!(sav_alloc(-1).is_null());
        assert!(sav_alloc(i32::MIN).is_null());
        // A rejection also empties the scratch, so a page that ignored the
        // NULL and called load_game anyway would parse "" (clean error),
        // never a stale prior text.
        SAV_BUF.with(|b| assert!(b.borrow().is_empty()));
        // In-range lengths (the cap itself included) still allocate.
        assert!(!sav_alloc(16).is_null());
        SAV_BUF.with(|b| assert_eq!(b.borrow().len(), 16));
        assert!(!sav_alloc(SAV_BUF_MAX).is_null());
        SAV_BUF.with(|b| assert_eq!(b.borrow().len(), SAV_BUF_MAX as usize));
    }

    /// The slot-listing primitive for the (sibling-branch) Load/Save menus:
    /// `extract_save_comment` parses a stored .sav from the scratch buffer and
    /// returns the M_ScanSaves-style display comment (underscores -> spaces).
    #[test]
    fn comment_extraction_for_slot_listings() {
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);
        step(0.05);
        console_toggle();
        run_console_line("save sl_comment");
        assert!(poll_save() > 0);
        let (_, text) = SAVE_CUR.with(|c| c.borrow().clone());

        SAV_BUF.with(|b| *b.borrow_mut() = text.into_bytes());
        let len = extract_save_comment();
        assert_eq!(len as usize, 39, "SAVEGAME_COMMENT_LENGTH");
        let comment = SAVE_COMMENT.with(|c| c.borrow().clone());
        // e1m1's worldspawn message is "the Slipgate Complex"; kills at col 22.
        assert!(comment.contains("Slipgate"), "{comment:?}");
        assert!(comment.contains("kills:"), "{comment:?}");
        assert!(!comment.contains('_'), "display form uses spaces: {comment:?}");

        // Garbage in the scratch -> 0, no panic.
        SAV_BUF.with(|b| *b.borrow_mut() = b"not a save".to_vec());
        assert_eq!(extract_save_comment(), 0);
    }

    // --- Options-menu liveness: sounds, keys, video, load/save, gamma --------

    /// The menu screen currently showing (test-side peek at the App menu).
    fn menu_screen() -> render::MenuScreen {
        APP.with(|c| c.borrow().as_ref().unwrap().menu.screen())
    }

    /// Drain the engine-side menu-sound queue completely (returns the drained
    /// WAV payload lengths, in order).
    fn drain_menu_sounds() -> Vec<i32> {
        let mut out = Vec::new();
        loop {
            let len = poll_menu_sound();
            if len <= 0 {
                break;
            }
            out.push(len);
        }
        out
    }

    #[test]
    fn poll_menu_sound_serves_the_real_wavs_and_respects_audio_gate() {
        assert_eq!(boot_attract(), 1);
        set_audio_ready(1);
        MENU_SND_QUEUE.with(|q| q.borrow_mut().clear());
        drain_menu_sounds(); // flush whatever boot queued (the open's menu2)

        // A cursor move queues misc/menu1.wav — the exact pak bytes.
        menu_down();
        let len = poll_menu_sound();
        assert!(len > 0, "menu navigation queues a local sound");
        let served = SND.with(|s| s.borrow().clone());
        let pak = pak().unwrap();
        let menu1 = pak.read_file("sound/misc/menu1.wav").unwrap().unwrap();
        assert_eq!(served, menu1, "cursor move serves misc/menu1.wav byte-for-byte");
        assert_eq!(poll_menu_sound(), 0, "queue drained");

        // Entering a submenu queues misc/menu2.wav (m_entersound).
        menu_up(); // back onto item 0
        drain_menu_sounds();
        menu_select(); // -> SinglePlayer
        let len = poll_menu_sound();
        assert!(len > 0);
        let served = SND.with(|s| s.borrow().clone());
        let menu2 = pak.read_file("sound/misc/menu2.wav").unwrap().unwrap();
        assert_eq!(served, menu2, "Enter serves misc/menu2.wav");
        drain_menu_sounds();

        // While audio isn't ready, queued menu sounds are DISCARDED (the
        // queue_sounds no-backlog rule), not saved up.
        set_audio_ready(0);
        menu_down();
        assert_eq!(poll_menu_sound(), 0, "no sound while audio is down");
        set_audio_ready(1);
        assert_eq!(poll_menu_sound(), 0, "pre-audio sounds were dropped, not queued");
    }

    #[test]
    fn key_down_drives_movement_through_bindings_and_always_run_swaps_speeds() {
        reset_queue();
        assert_eq!(boot(), 1);
        close_menu();

        // Default binding: w = +forward at cl_forwardspeed 400 — Always Run
        // defaults ON in this port (Menu's DEVIATION note).
        key_down(i32::from(b'w'));
        step(0.05);
        let fwd_run = walk_mut(|w| w.key_move.fwd);
        assert_eq!(fwd_run, 400.0, "+forward runs at cl_forwardspeed 400 (Always Run default)");

        // Hold +speed (Shift, default.cfg): cl_movespeedkey doubles it.
        key_down(134); // K_SHIFT
        step(0.05);
        assert_eq!(walk_mut(|w| w.key_move.fwd), 800.0, "+speed doubles via cl_movespeedkey");
        key_up(134);

        // The player really moves (the server clamps wishspeed to sv_maxspeed
        // 320, so 400 is 320 effective — exactly WinQuake's run).
        let (x0, y0) = (listener_x(), listener_y());
        for _ in 0..20 {
            step(0.05);
        }
        let dist = ((listener_x() - x0).powi(2) + (listener_y() - y0).powi(2)).sqrt();
        assert!(dist > 100.0, "held +forward displaces the player (moved {dist:.1}u)");

        // Always Run (Options row 8) swaps cl_forwardspeed 400 -> 200.
        menu_cancel(); // open the menu
        menu_down();
        menu_down();
        menu_select(); // -> Options (Main cursor 2)
        for _ in 0..8 {
            menu_down(); // ROW_ALWAYSRUN (M_AdjustSliders case 8)
        }
        menu_right(); // toggle OFF
        menu_cancel(); // Options -> Main
        menu_cancel(); // Main -> closed
        assert_eq!(menu_visible(), 0);
        step(0.05);
        assert_eq!(
            walk_mut(|w| w.key_move.fwd),
            200.0,
            "toggling Always Run off drops the walk to 200"
        );

        // Releasing the key stops the contribution.
        key_up(i32::from(b'w'));
        step(0.05);
        assert_eq!(walk_mut(|w| w.key_move.fwd), 0.0, "key_up ends +forward");
    }

    #[test]
    fn invert_mouse_flips_pitch_and_lookspring_recentres_on_unlock() {
        reset_queue();
        assert_eq!(boot(), 1);
        close_menu();

        // Mouse pulled down (positive movementY) looks DOWN (positive pitch).
        walk_mut(|w| w.pitch = 0.0);
        mouse_move(0.0, 100.0);
        let p = player_pitch();
        assert!(p > 0.0, "non-inverted mouse-down looks down (pitch {p})");

        // Toggle Invert Mouse (Options row 9): the m_pitch sign flips.
        menu_cancel();
        menu_down();
        menu_down();
        menu_select(); // -> Options
        for _ in 0..9 {
            menu_down(); // ROW_INVERTMOUSE (M_AdjustSliders case 9)
        }
        menu_right();
        menu_cancel();
        menu_cancel();
        walk_mut(|w| w.pitch = 0.0);
        mouse_move(0.0, 100.0);
        let p = player_pitch();
        assert!(p < 0.0, "inverted mouse-down looks up (pitch {p})");

        // Lookspring OFF: pointer unlock leaves the pitch alone.
        walk_mut(|w| w.pitch = -40.0);
        pointer_unlocked();
        for _ in 0..10 {
            step(0.05);
        }
        assert_eq!(player_pitch(), -40.0, "no lookspring, no recentre");

        // Lookspring ON (row 10): unlock starts the V_StartPitchDrift recentre.
        menu_cancel();
        menu_down();
        menu_down();
        menu_select();
        for _ in 0..10 {
            menu_down(); // ROW_LOOKSPRING (M_AdjustSliders case 10)
        }
        menu_right();
        menu_cancel();
        menu_cancel();
        walk_mut(|w| w.pitch = -40.0);
        pointer_unlocked();
        for _ in 0..30 {
            step(0.05);
        }
        let p = player_pitch();
        assert!(p.abs() < 0.5, "lookspring recentred the view (pitch {p})");
        // ...and a mouse move stops an in-flight drift (V_StopPitchDrift).
        walk_mut(|w| w.pitch = -40.0);
        pointer_unlocked();
        mouse_move(0.0, 1.0);
        for _ in 0..10 {
            step(0.05);
        }
        assert!(player_pitch() < -30.0, "mlook motion stops the drift");
    }

    #[test]
    fn lookstrafe_routes_mouse_x_to_sidemove() {
        reset_queue();
        assert_eq!(boot(), 1);
        close_menu();

        // Default: mouse X turns (yaw changes, no sidemove accumulates).
        let yaw0 = walk_mut(|w| w.yaw);
        mouse_move(100.0, 0.0);
        assert!(walk_mut(|w| w.yaw) < yaw0, "mouse-right turns right (yaw -= m_yaw*mx)");
        assert_eq!(walk_mut(|w| w.mouse_side), 0.0);

        // Lookstrafe ON (Options row 11): mouse X strafes instead.
        menu_cancel();
        menu_down();
        menu_down();
        menu_select();
        for _ in 0..11 {
            menu_down(); // ROW_LOOKSTRAFE (M_AdjustSliders case 11)
        }
        menu_right();
        menu_cancel();
        menu_cancel();
        let yaw1 = walk_mut(|w| w.yaw);
        mouse_move(100.0, 0.0);
        assert_eq!(walk_mut(|w| w.yaw), yaw1, "lookstrafe holds the yaw still");
        // sidemove += m_side * (mx * sensitivity 3) = 0.8 * 300 = 240.
        assert_eq!(walk_mut(|w| w.mouse_side), 240.0, "mouse X became sidemove units");
        // The accumulator drains into the next frame's cmd.
        step(0.05);
        assert_eq!(walk_mut(|w| w.mouse_side), 0.0, "step drained the strafe units");
    }

    #[test]
    fn video_menu_applies_a_preset_through_the_resolution_plumbing() {
        reset_queue();
        assert_eq!(boot(), 1);
        set_resolution(320, 200); // preset 0
        // boot() opened the menu on Main. Navigate: Options (cursor 2) ->
        // Video Options (row 12) -> down one mode -> Enter applies it.
        menu_down();
        menu_down();
        menu_select(); // -> Options
        for _ in 0..12 {
            menu_down(); // ROW_VIDEO
        }
        menu_select(); // -> Video mode list (cursor on the current preset, 0)
        assert_eq!(menu_screen(), render::MenuScreen::Video);
        menu_down(); // preset 1 = 480x300
        menu_select(); // VID_MenuKey K_ENTER -> VID_SetMode
        assert_eq!((width(), height()), (480, 300), "Enter applied the highlighted mode");
        assert_eq!(menu_screen(), render::MenuScreen::Video, "the list stays up");
        // Esc returns to Options (VID_MenuKey K_ESCAPE -> M_Menu_Options_f).
        menu_cancel();
        assert_eq!(menu_screen(), render::MenuScreen::Options);
    }

    #[test]
    fn load_save_screens_gate_and_emit_actions_via_exports() {
        reset_queue();
        // ATTRACT (demo) mode: no game running -> Save refuses to open.
        assert_eq!(boot_attract(), 1);
        step(0.05); // sync game_active (mode 1 -> false)
        menu_select(); // Main item 0 -> SinglePlayer
        menu_down();
        menu_down(); // cursor 2 = Save
        menu_select();
        assert_eq!(
            menu_screen(),
            render::MenuScreen::SinglePlayer,
            "Save refuses without a running game (M_Menu_Save_f's sv.active gate)"
        );
        // Load always opens; every slot is unused, so Enter does nothing.
        menu_up(); // cursor 1 = Load
        menu_select();
        assert_eq!(menu_screen(), render::MenuScreen::Load);
        menu_select(); // unused slot: M_Load_Key's !loadable return
        assert_eq!(menu_screen(), render::MenuScreen::Load, "unused slot stays put");
        assert_eq!(menu_visible(), 1);
        // Esc backs out to SinglePlayer.
        menu_cancel();
        assert_eq!(menu_screen(), render::MenuScreen::SinglePlayer);

        // WALK mode: the game runs -> Save opens; Enter emits SaveSlot (a
        // host no-op until the savegame engine lands) and closes the menu.
        assert_eq!(boot(), 1);
        step(0.05); // sync game_active (walk, no intermission -> true)
        menu_select(); // -> SinglePlayer
        menu_down();
        menu_down();
        menu_select(); // -> Save
        assert_eq!(menu_screen(), render::MenuScreen::Save);
        menu_down(); // slot 1
        menu_select(); // SaveSlot(1): menu closes like the C, host no-ops
        assert_eq!(menu_visible(), 0, "Save Enter closes the menu");
        // The world is untouched by the no-op (player still alive on e1m1).
        assert!(player_field("health") > 0.0);
    }

    #[test]
    fn keys_screen_rebinds_forward_through_the_exports() {
        reset_queue();
        assert_eq!(boot(), 1);
        // Navigate: Options -> Customize controls (row 0).
        menu_down();
        menu_down();
        menu_select(); // -> Options
        menu_select(); // ROW_CONTROLS -> Keys screen
        assert_eq!(menu_screen(), render::MenuScreen::Keys);
        // Move to the "+forward" row (BIND_FORWARD = 3) and grab.
        for _ in 0..3 {
            menu_down();
        }
        assert_eq!(menu_bind_grabbing(), 0);
        menu_select();
        assert_eq!(menu_bind_grabbing(), 1, "Enter starts the bind grab");
        // +forward had two keys (w + UPARROW): the C unbinds them, then binds
        // the grabbed key.
        menu_bind_key(i32::from(b'o'));
        assert_eq!(menu_bind_grabbing(), 0);
        // Close the menu (Keys -> Options -> Main -> closed).
        menu_cancel();
        menu_cancel();
        menu_cancel();
        assert_eq!(menu_visible(), 0);
        // The new key drives +forward; the old one no longer does.
        key_down(i32::from(b'o'));
        step(0.05);
        // (400: Always Run defaults on, so +forward moves at the run speed.)
        assert_eq!(walk_mut(|w| w.key_move.fwd), 400.0, "rebound key moves forward");
        key_up(i32::from(b'o'));
        key_down(i32::from(b'w'));
        step(0.05);
        assert_eq!(walk_mut(|w| w.key_move.fwd), 0.0, "the old key was unbound");
        key_up(i32::from(b'w'));
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

