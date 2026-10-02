//! The game client — the part of Quake that in id's source is `cl_*.c`,
//! `view.c` and the client half of `host.c`/`host_cmd.c`: it turns the local
//! server's state (or a recorded demo's stream) into a finished screen and
//! the calls it makes into the platform's sound layer. The browser shell
//! (quake-wasm) runs it for the page; `quaketool play` runs it natively.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//!
//! ## Layout
//!
//! | module       | id counterpart                    | what |
//! |--------------|-----------------------------------|------|
//! | this file    | client.h, host.c                  | the client state per mode — the live [`Walk`] (with the local server it drives) and the recorded [`DemoPlay`] — and what a frame takes and gives: [`Vid`], [`ClientFrame`], [`SoundCall`]; the frame-timer and view hooks |
//! | [`cl_main`]  | cl_main.c, cl_parse.c, view.c, screen.c | [`cl_main::walk_frame`]: the live client frame — `CL_SendMove` into the server tick, the client side of `CL_ParseServerMessage`, `CL_RelinkEntities`, `V_CalcRefdef`, `SCR_UpdateScreen`'s view, blends and status bar |
//! | [`cl_demo`]  | cl_demo.c, cl_parse.c, view.c     | `CL_PlayDemo_f`'s build, quake.rc's demo loop, [`cl_demo::demo_frame`]: the recorded stream rendered like live play |
//! | [`cl_tent`]  | cl_tent.c, r_part.c               | temp-entity effects (explosions, impacts, their sounds), the model-flag trails |
//! | [`cl_input`] | cl_input.c                        | [`cl_input::KeyMove`]: `CL_BaseMove`/`CL_AdjustAngles` over the held keys and bindings, the `cl_*` move cvars |
//! | [`in_win`]   | in_win.c (the joystick)           | [`in_win::Joystick`]: a pad as winmm's joystick — `IN_Commands`' `JOY`/`AUX` keys, `IN_JoyMove`'s move and turn with the `joy*` cvars — and the 2026 pad's stick shaping, menu keys and rumble |
//! | [`view`]     | view.c                            | `V_ParseDamage`, the damage kick, `V_BonusFlash_f`, the item get-times (the renderer's half of view.c is `render`'s) |
//! | [`host`]     | host.c                            | `Host_FilterTime`: the 72 fps gate and the frame time it hands the game |
//! | [`host_cmd`] | host_cmd.c                        | the level loads (`map`, changelevel, restart, a savegame's rebuild) and the cheats (god, noclip, fly, kill, give, impulse) |
//! | [`lerpmove`] | (QuakeSpasm's `r_lerpmove`)       | the 2026 extra: monsters glide between their steps ([`lerpmove::LerpMove`]) |
//! | [`lerpmodels`] | (QuakeSpasm's `r_lerpmodels`)  | the 2026 extra: an alias model's animation blends between frames ([`lerpmodels::LerpModels`]) |
//!
//! ## What a frame takes and gives
//!
//! A frame — [`cl_main::walk_frame`] or [`cl_demo::demo_frame`] — takes the
//! client state, the frame time (from [`host::host_filter_time`]), whether
//! the game is paused behind the menu or console, and the [`Vid`] it draws
//! (the mode, the display's aspect, the renderer extra). It returns a
//! [`ClientFrame`]: the screen, `cl.cshifts` for the platform to present it
//! through (after its menu and console), and the calls it made into the
//! sound layer — [`SoundCall`]s, in call order: `S_StartSound` batches,
//! `S_StopSound`, `S_StopAllSounds` and the `S_StaticSound` loops of a level
//! change, and `S_Update`'s listener pose and ambient leaf. The level loads
//! record the same calls into a caller's `Vec`. What else the host needs it
//! reads off the state, as id's host reads `cl`: the printed text for its
//! console ([`Walk::notify`]), `pending_sellscreen`, `intermission`, and
//! whether a QuakeC error ended the game ([`Walk::host_error`]). A
//! platform may install frame timers ([`set_lap_hook`]) and a hook on the
//! finished 3-D view ([`set_view_hook`]); none is installed by default.

pub mod cl_demo;
pub mod cl_input;
pub mod cl_main;
pub mod cl_tent;
pub mod host;
pub mod host_cmd;
pub mod in_win;
pub mod lerpmodels;
pub mod lerpmove;
pub mod view;

use std::cell::Cell;
use std::collections::HashMap;

use crate::bsp::{Bsp, NUM_AMBIENTS};
use crate::console::ConNotify;
use crate::demo::Demo;
use crate::dlight::DynamicLights;
use crate::mdl::Mdl;
use crate::cd_audio::CdCall;
use crate::pak::Pak;
use crate::particles::{Lcg, ParticleSystem, TrailHead};
use crate::render;
use crate::server::{Server, SoundEvent, StaticSound};
use crate::stepping::{Stepping, Tick72};
use crate::tent::{BeamSegment, Beams};
use crate::wad::Qpic;
use cl_input::{clamp_pitch, KeyMove};
use lerpmodels::{FrameLerps, LerpModels};
use lerpmove::{LerpMove, StepGlides};

// ---------------------------------------------------------------------------
// The client state
// ---------------------------------------------------------------------------

/// Interactive walk state: a live server ticked every frame, rendered from the
/// player edict. Monster thinks advance their animation frames and move them,
/// and the sound queue surfaces the events they fire.
pub struct Walk {
    pub server: Server,
    /// A second copy of the map BSP for rendering (the server owns its own copy
    /// inside the world host).
    pub bsp: Bsp,
    /// The renderer drawing `bsp` (begun on it: `R_NewMap` whenever `bsp`
    /// changes), with its caches, z-buffer and video cvars.
    pub renderer: render::Renderer,
    pub palette: [[u8; 3]; 256],
    /// The parsed `gfx.wad` (sbar + digit pics) for the status-bar HUD overlay,
    /// or `None` if the archive lacked/could not parse it. Parsed once at boot so
    /// the per-frame HUD draw is allocation-light.
    pub gfx_wad: Option<crate::wad::Wad2>,
    /// `gfx/colormap.lmp` — the 64x256 shade LUT for faithful colormap-indexed
    /// wall lighting (Quake never overbrights). `None` falls back to the linear
    /// brightness multiply. Loaded once at boot.
    pub colormap: Option<Vec<u8>>,
    /// The `conchars` font (extracted once from `gfx_wad`) for the on-screen
    /// message overlay — centerprint (centered) + notify lines (top-left).
    pub conchars: Option<Qpic>,
    /// The archive, kept open so sound samples load on demand as events fire.
    pub pak: Pak,
    /// Parsed alias models keyed by in-pak name (`None` = absent/unparseable).
    pub model_cache: HashMap<String, Option<Mdl>>,
    /// Parsed *external brush* models keyed by in-pak name (`None` =
    /// absent/unparseable). These are Quake's standalone `maps/b_*.bsp` item
    /// boxes (explosive box, ammo/health boxes) that items `setmodel()` to at
    /// runtime. Cached like `model_cache` so a box parses once and backs every
    /// instance of that item; rendered via [`render::ExternalBModel`].
    pub bmodel_cache: HashMap<String, Option<Bsp>>,
    /// Parsed sprite models (`.spr`) keyed by name, cached like `model_cache` so a
    /// sprite (the explosion flash, bubbles) parses once and backs every instance.
    pub sprite_cache: HashMap<String, Option<crate::spr::Sprite>>,
    /// Per-entity previous render origin, keyed by edict index — the source point
    /// for R_RocketTrail (rockets/grenades/gibs trail from their old origin to the
    /// new one each frame). Defaults to the current origin the first time an
    /// entity is seen, so there's no spurious trail on spawn.
    pub trail_org: HashMap<i32, TrailHead>,
    /// Quake's `tracercount` (CL_RelinkEntities `static int`): alternates the
    /// tracer-trail offset direction; threaded across `spawn_rocket_trail` calls.
    pub tracercount: u32,
    /// The world map's in-pak path (e.g. `maps/e1m1.bsp`). An entity whose
    /// `model` equals this is the worldspawn brush — never loaded as an external
    /// box (it is already drawn as the world).
    pub map_name: String,
    /// The spawn parms captured when this level was ENTERED (the alive player's
    /// inventory at level start). A single-player respawn (`localcmd("restart")`)
    /// reloads the current level with THESE, since a dead player's state is empty.
    pub entry_parms: [f32; crate::server::NUM_SPAWN_PARMS],
    pub player: i32,
    pub yaw: f32,
    pub pitch: f32,
    pub in_fwd: f32,
    pub in_side: f32,
    pub in_attack: bool,
    /// Whether the jump key is held (UserCmd button bit 1 -> the player's
    /// `button2`, which the QuakeC PlayerJump reads to leap when on the ground).
    pub in_jump: bool,
    /// Whether the swim-down key (`c`) is held: drives `UserCmd.upmove` negative,
    /// which `SV_WaterMove` reads to sink. Ignored out of water (the WALK air
    /// move zeroes the vertical wish). Swim-UP reuses `in_jump` (Space) the same
    /// way — in water it pushes up, on land it just jumps.
    pub in_down: bool,
    /// A one-shot impulse (weapon switch etc.) queued by `set_impulse`, applied
    /// to the next `walk_frame` UserCmd then cleared — matching how Quake's
    /// `impulse` console command fires once. 0 means "no impulse this frame".
    pub next_impulse: i32,
    /// This frame's bindings-derived keyboard input (CL_BaseMove/CL_AdjustAngles
    /// over the page-held keys), refreshed by `step` before `walk_frame` runs.
    pub key_move: KeyMove,
    /// The `viewsize` cvar this frame (the Options "Screen size" slider),
    /// refreshed by `step` from the menu before stepping, like `key_move`:
    /// [`render::calc_refdef`] turns it into the 3-D view rectangle and how
    /// much status bar shows.
    pub viewsize: f32,
    /// The `crosshair` cvar this frame: `V_RenderView` draws the `+` over
    /// the finished view ([`render::draw_crosshair`]). Set like `viewsize`.
    pub crosshair: bool,
    /// How this frame steps the game ([`Stepping`]): Classic, id's per-frame
    /// code, unless the host runs uncapped. Set by the host each frame, like
    /// `key_move`.
    pub stepping: Stepping,
    /// How monsters are drawn between their steps ([`LerpMove`], `r_lerpmove`):
    /// Classic unless the host turns the extra on. Set by the host each
    /// frame, like `stepping`.
    pub lerpmove: LerpMove,
    /// The monsters' glides while [`LerpMove::Smooth`] is on.
    pub glides: StepGlides,
    /// Whether an alias model's animation blends between frames
    /// ([`LerpModels`], `r_lerpmodels`): Classic unless the host turns the
    /// extra on. Set by the host each frame, like `lerpmove`.
    pub lerpmodels: LerpModels,
    /// Every drawn alias entity's (and the view weapon's) animation blend
    /// while [`LerpModels::Smooth`] is on.
    pub frame_lerps: FrameLerps,
    /// Accumulated mouse-strafe sidemove units (in_win.c IN_MouseMove's
    /// `cmd->sidemove += m_side.value * mouse_x` when lookstrafe / +strafe route
    /// mouse X away from yaw). Drained into the next UserCmd then cleared.
    pub mouse_side: f32,
    /// Accumulated mouse forwardmove units (IN_MouseMove's else branch:
    /// `cmd->forwardmove -= m_forward.value * mouse_y` while +strafe holds mouse
    /// Y out of the pitch path). Drained like `mouse_side`.
    pub mouse_fwd: f32,
    /// Pitch drift active (view.c `!cl.nodrift`): centerview or a lookspring
    /// pointer-unlock started it; the view re-levels at `pitch_vel` deg/sec until
    /// it reaches 0 or mouse/keyboard look stops it (V_StopPitchDrift).
    pub pitch_drift: bool,
    /// `cl.pitchvel` — the drift rate, seeded with [`V_CENTERSPEED`](cl_input::V_CENTERSPEED) and
    /// accelerated by it each second while drifting (V_DriftPitch).
    pub pitch_vel: f32,
    /// Full-screen damage-flash intensity (Quake's `CSHIFT_DAMAGE` percent,
    /// 0..150): bumped by each svc_damage (V_ParseDamage) and faded each frame.
    pub damage_blend: f32,
    /// The damage-flash tint colour (`V_ParseDamage` picks (200,100,100) when armour
    /// absorbs most, (220,50,50) for armour-only, (255,0,0) for pure blood).
    pub damage_color: [u8; 3],
    /// `cl.cshifts[CSHIFT_BONUS].percent`: the gold pickup flash a stuffed
    /// `bf` sets to 50 (V_BonusFlash_f), dropped `dt*100` per frame.
    pub bonus_blend: f32,
    /// Counts the 1/72 s ticks the uncapped client fades the flashes by
    /// ([`view::fade_cshifts`]).
    pub fade_clock: Tick72,
    /// `v_dmg_time` / `v_dmg_roll` / `v_dmg_pitch` (view.c): the directional
    /// view kick of the last svc_damage, decaying over `v_kicktime`.
    pub v_dmg_time: f32,
    pub v_dmg_roll: f32,
    pub v_dmg_pitch: f32,
    /// `cl.faceanimtime` (V_ParseDamage: `cl.time + 0.2`, on the server clock
    /// like the HUD's `time`): the status bar shows the pain face until then.
    pub faceanimtime: f32,
    /// `V_ParseDamage`'s `count`s since the platform last took them: the 2026
    /// pad's rumble on damage ([`in_win::Rumble::damage`]). Nothing in the
    /// game reads it.
    pub damage_count: f32,
    /// `cl.items` as last received and `cl.item_gettime[]` (CL_ParseClientdata,
    /// server clock): the new-weapon icon flash. A level start (CL_ClearState)
    /// seeds the items the player spawns with and zeroes the get-times, so
    /// what the player carries does not flash (`view::stamp_item_gettime`).
    pub cl_items: i32,
    pub item_gettime: [f32; 32],
    /// Stair-step view smoothing accumulator (`view.c` V_CalcRefdef `oldz`): the eye
    /// Z lags the player Z by up to 12 units while climbing so stairs glide instead
    /// of jolting. NaN until the first frame establishes it.
    pub oldz: f32,
    /// Current centered message (`centerprint`) + the clock time it expires at
    /// (Quake's `scr_centertime` ~2s); replaced by the next centerprint. Drawn
    /// centered over the view.
    pub centerprint: Option<(String, f32)>,
    /// The top-left notify lines (`bprint`/`sprint` through Con_Print, shown by
    /// Con_DrawNotify), on the host clock.
    pub notify: ConNotify,
    /// `cl.time` (seconds). On a local server `CL_LerpPoint` snaps it to the
    /// server's message time, `sv.time` after the frame's physics, so it is
    /// the server's clock: about 1.2 s at a spawn (SV_SpawnServer's 1.0 + the
    /// signon frames), the save's time after a load, and it stops with the
    /// server while single player is paused behind the menu/console. Drives
    /// the sky, liquids, underwater warp, texture/alias animation, light
    /// styles, particles, dlight decay, the rotating pickups, the bob and the
    /// intermission sway.
    pub clock: f32,
    /// Host time (seconds): advanced by every frame's `dt`, paused or not. The
    /// notify lines (Con_DrawNotify ages them in `realtime`) and the centerprint
    /// (SCR_CheckDrawCenterString counts `scr_centertime_off` down by
    /// `host_frametime`) expire on this clock, so they keep timing out behind the
    /// menu as in the C.
    pub host_time: f32,
    /// `host_time` to double precision, which the uncapped client keeps it by
    /// ([`crate::stepping::advance_clock`]).
    pub host_clock: f64,
    /// Live engine particles (the `particle()` builtin's effect). Bursts the
    /// QuakeC fires each frame are drained into this pool, aged under gravity,
    /// and drawn into the scene sharing its z-buffer.
    pub particles: ParticleSystem,
    /// Deterministic RNG for particle spawns (no `rand` crate; std-only).
    pub prng: Lcg,
    /// Live dynamic lights (explosions, muzzle flashes, EF_* lights). Allocated
    /// each frame from the drained temp entities + the server's entity_dlights,
    /// decayed under `advance`, and passed to the renderer to light the walls.
    pub dlights: DynamicLights,
    /// The beam temp-entity slots (`cl_beams`): lightning bolts the drained
    /// `TE_LIGHTNING1/2/3` / `TE_BEAM` events refresh ([`Beams::parse_beam`])
    /// and [`walk_frame`](cl_main::walk_frame) expands into bolt-model instances each frame
    /// (`CL_UpdateTEnts`). Cleared on changelevel/restart (`CL_ClearState`).
    pub beams: Beams,
    /// Reused per-frame scratch for the expanded beam pieces (no per-frame
    /// allocation on the common no-beam frames; `Beams::update` clears it).
    pub beam_scratch: Vec<BeamSegment>,
    /// `cl.intermission` (client.h): 0 = playing, 1 = the level-complete stats
    /// overlay (svc_intermission), 2 = the episode finale text + plaque
    /// (svc_finale), 3 = cutscene text only (svc_cutscene). While non-zero the
    /// view is the QC-placed intermission camera (V_CalcIntermissionRefdef): no
    /// bob/roll/punch, no viewmodel, no status bar.
    pub intermission: u8,
    /// `cl.completed_time` — the clock latched when the intermission started
    /// (the overlay's minutes:seconds completion time).
    pub completed_time: f32,
    /// The `svc_finale`/`svc_cutscene` text (`SCR_CenterPrint`'d in the C),
    /// revealed at `scr_printspeed` (8) chars/sec from `finale_start`.
    pub finale_text: String,
    /// `scr_centertime_start` — the clock when the finale text began revealing.
    pub finale_start: f32,
    /// `svc_sellscreen` arrived this frame: the C ran `Cmd_ExecuteString("help")`,
    /// i.e. popped the Help/Ordering menu — the `step` dispatcher (which owns the
    /// menu) takes this flag and opens it.
    pub pending_sellscreen: bool,
    /// `Host_Error`'s message once this game has ended in one — a QuakeC
    /// runtime error: "Program error" ([`host::host_error`]). What id printed
    /// on the way is already in [`Walk::notify`]'s printed text. The host does
    /// the rest of `Host_Error`: `CL_Disconnect` (drop this walk, whose server
    /// has shut down), `cls.demonum = -1`, and its console comes down over
    /// the disconnected screen. [`cl_main::walk_frame`] runs nothing more.
    pub host_error: Option<String>,
    /// `gfx/complete.lmp` — the "Level Complete" banner (Sbar_IntermissionOverlay).
    pub pic_complete: Option<Qpic>,
    /// `gfx/inter.lmp` — the Time/Secrets/Kills intermission plaque.
    pub pic_inter: Option<Qpic>,
    /// `gfx/finale.lmp` — the finale plaque (Sbar_FinaleOverlay).
    pub pic_finale: Option<Qpic>,
    /// `gfx/pause.lmp` — the plaque `SCR_DrawPause` shows while paused.
    pub pic_pause: Option<Qpic>,
}

/// Recorded-demo playback state.
pub struct DemoPlay {
    pub bsp: Bsp,
    /// The renderer drawing `bsp` (begun on it), as [`Walk::renderer`].
    pub renderer: render::Renderer,
    pub palette: [[u8; 3]; 256],
    pub demo: Demo,
    /// The archive, kept open so the recorded `svc_sound` one-shots can load
    /// their WAV bytes on demand — the demo's audio runs through the SAME
    /// `S_StartSound` path live play uses.
    pub pak: Pak,
    /// Parsed model per precache index (None for non-`.mdl` / missing).
    pub models: Vec<Option<Mdl>>,
    /// Parsed sprite per precache index (None for non-`.spr` / missing); the boot
    /// demo's explosion flashes (s_explod.spr) render from these.
    pub sprites: Vec<Option<crate::spr::Sprite>>,
    /// `gfx/colormap.lmp` — the 64-row shade LUT. The demo path must thread it like
    /// the live walk does, else world surfaces render overbright (linear fallback)
    /// instead of through id's no-overbright colormap shading.
    pub colormap: Option<Vec<u8>>,
    pub colors: Vec<[u8; 3]>,
    /// `cl.time`: the client's clock, which `demo_frame` advances by the host
    /// frame time and `CL_LerpPoint` keeps between the two newest messages
    /// read (a double, as in client.h).
    pub time: f64,
    /// `cl.oldtime`: `cl.time` before this frame advanced it (the particles
    /// and the stair smoothing step by `cl.time - cl.oldtime`).
    pub oldtime: f64,
    /// The newest recorded message read (`cl.mtime[0]`'s): an index into
    /// `demo.frames`.
    pub idx: usize,
    /// What this frame's `CL_RelinkEntities` drew: the clock, the camera and
    /// every entity between the two newest messages.
    pub view: cl_demo::DemoView,
    /// Live particles replayed from the recorded `svc_particle` / temp-entity
    /// stream: each message's effects are spawned ONCE, in the frame that reads
    /// it, then the pool is aged under gravity and drawn into the scene (sharing
    /// its z-buffer) — so the demo shows blood, gunshot puffs and explosions just
    /// like [`walk_frame`](cl_main::walk_frame) does for live play.
    pub particles: ParticleSystem,
    /// Deterministic RNG for the demo's particle spawns (std-only, like Walk).
    pub prng: Lcg,
    /// The message whose effects were last spawned, so each message's bursts
    /// spawn once, in the frame that reads it; `None` until the first frame of
    /// playback reads one.
    pub last_spawned_idx: Option<usize>,
    /// The beam temp-entity slots (`cl_beams`) replayed from the recorded
    /// `TE_LIGHTNING1/2/3` / `TE_BEAM` stream; expanded into bolt-model
    /// instances each frame like the live walk. Cleared on the demo loop wrap.
    pub beams: Beams,
    /// Reused per-frame scratch for the expanded beam pieces.
    pub beam_scratch: Vec<BeamSegment>,
    /// `gfx.wad` (the big digit pics) for a recorded intermission's stats
    /// overlay; `None` degrades to no overlay, never a panic.
    pub gfx_wad: Option<crate::wad::Wad2>,
    /// The conchars font for a recorded finale's revealed center string.
    pub conchars: Option<Qpic>,
    /// `gfx/complete.lmp` / `gfx/inter.lmp` / `gfx/finale.lmp` — the plaques a
    /// recorded intermission/finale frame draws (Sbar_Intermission/FinaleOverlay).
    pub pic_complete: Option<Qpic>,
    pub pic_inter: Option<Qpic>,
    pub pic_finale: Option<Qpic>,
    /// `gfx/pause.lmp`, for a recorded `svc_setpause` (`SCR_DrawPause`).
    pub pic_pause: Option<Qpic>,
    /// `cl.cshifts[CSHIFT_DAMAGE].percent` for the recorded POV: bumped by each
    /// recorded `svc_damage` (V_ParseDamage: `+= 3*count`, clamped 0..150) and
    /// faded `dt*150` per rendered frame (V_UpdatePalette), exactly like the
    /// live walk's damage flash.
    pub damage_blend: f32,
    /// The damage tint V_ParseDamage picked (armour-dominant pink / armour
    /// orange-red / pure-blood red).
    pub damage_color: [u8; 3],
    /// `cl.cshifts[CSHIFT_BONUS].percent` for the recorded POV: a recorded
    /// `svc_stufftext "bf"` sets it to 50 (V_BonusFlash_f), faded `dt*100`.
    pub bonus_blend: f32,
    /// Counts the 1/72 s ticks the uncapped client fades the flashes by
    /// ([`view::fade_cshifts`]).
    pub fade_clock: Tick72,
    /// `v_dmg_time` / `v_dmg_roll` / `v_dmg_pitch` (view.c): the directional
    /// view kick a recorded svc_damage applies, decaying over `v_kicktime`.
    pub v_dmg_time: f32,
    pub v_dmg_roll: f32,
    pub v_dmg_pitch: f32,
    /// Stair-step smoothing accumulator (V_CalcRefdef `oldz`) for the recorded
    /// view entity; NaN until the first frame establishes it.
    pub oldz: f32,
    /// Current centerprint + expiry (recorded `svc_centerprint`, scr_centertime
    /// ~2 s on the demo's recorded frame clock).
    pub centerprint: Option<(String, f32)>,
    /// The notify lines (recorded `svc_print` through Con_Print), on the
    /// recorded clock.
    pub notify: ConNotify,
    /// The `viewsize` cvar this frame (the Options "Screen size" slider),
    /// refreshed by `step` from the menu before stepping, like `key_move`:
    /// [`render::calc_refdef`] turns it into the 3-D view rectangle and how
    /// much status bar shows.
    pub viewsize: f32,
    /// The `crosshair` cvar this frame (see `Walk::crosshair`).
    pub crosshair: bool,
    /// How this frame steps playback ([`Stepping`]), set by the host each
    /// frame like `viewsize`.
    pub stepping: Stepping,
    /// How the recorded monsters are drawn between their steps
    /// ([`LerpMove`]), set by the host each frame like `stepping`.
    pub lerpmove: LerpMove,
    /// The recorded monsters' glides while [`LerpMove::Smooth`] is on.
    pub glides: StepGlides,
    /// Whether the recorded entities' (and the view weapon's) animation
    /// blends between frames ([`LerpModels`]), set by the host each frame
    /// like `lerpmove`.
    pub lerpmodels: LerpModels,
    /// Every drawn entity's animation blend while [`LerpModels::Smooth`] is on.
    pub frame_lerps: FrameLerps,
    /// The `sv_gravity` cvar, which `R_DrawParticles` reads in playback too
    /// (`grav = frametime * sv_gravity * 0.05`): 800, or what the last map the
    /// host ran set it to (e1m8's worldspawn: 100; id's cvar outlives the map)
    /// — the host sets it from its last server's
    /// ([`crate::server::Server::sv_gravity`]), not the recording's.
    pub sv_gravity: f32,
    /// Each relinked entity's origin as last rendered (CL_RelinkEntities'
    /// `oldorg`), keyed by entity number, for the model-flag trails; an entity
    /// missing from a frame is forgotten (its next sighting is a forcelink).
    pub trail_org: HashMap<i32, TrailHead>,
    /// R_RocketTrail's `static int tracercount` for the demo's tracer trails.
    pub tracercount: u32,
    /// Which of [`DEMOS`](cl_demo::DEMOS) this is (the next one follows it, CL_NextDemo).
    pub demonum: usize,
    /// `sb_showscores` (`+showscores`, Tab held): Sbar_Draw shows the solo
    /// scoreboard during playback too. Refreshed by `step` like `viewsize`.
    pub show_scores: bool,
    /// `cl.faceanimtime` (V_ParseDamage: `cl.time + 0.2`): the status bar
    /// shows the pain face until then.
    pub faceanimtime: f32,
    /// `cl.items` as last shown and `cl.item_gettime[]` on the recorded clock
    /// (CL_ParseClientdata): the new-weapon icon flash. Playback start and the
    /// loop wrap (CL_ClearState) seed the first frame's items (the signon's
    /// clientdata) with the get-times at 0: nothing flashes at the start
    /// (`view::stamp_item_gettime`).
    pub cl_items: i32,
    pub item_gettime: [f32; 32],
}

impl DemoPlay {
    /// The last frame has been shown: the recording is over (id's demos end
    /// with `svc_disconnect`, which the parser stops at).
    pub fn at_end(&self) -> bool {
        self.idx + 1 >= self.demo.frames.len()
    }

    /// A playback of `demo` over `bsp` at its first frame: no models, sprites,
    /// colormap or overlay pics yet (the caller loads what it has), and every
    /// per-playback field — clocks, particles, beams, view shifts, messages —
    /// at its clean-slate default.
    pub fn new(pak: Pak, bsp: Bsp, palette: [[u8; 3]; 256], demo: Demo) -> DemoPlay {
        // CL_ClearState: the signon and frame 0's block are parsed in one
        // CL_ReadFromServer, before the first CL_LerpPoint, so their items
        // are stamped at about host_frametime and never flash.
        let cl_items = demo.frames.first().map_or(0, |f| f.client.items);
        let mut renderer = render::Renderer::new();
        renderer.begin_map(&bsp);
        DemoPlay {
            bsp,
            renderer,
            palette,
            demo,
            pak,
            models: Vec::new(),
            sprites: Vec::new(),
            colormap: None,
            colors: Vec::new(),
            time: 0.0,
            oldtime: 0.0,
            idx: 0,
            view: cl_demo::DemoView::default(),
            particles: ParticleSystem::new(),
            prng: Lcg::new(0x9E37_79B9),
            last_spawned_idx: None,
            beams: Beams::new(),
            beam_scratch: Vec::new(),
            gfx_wad: None,
            conchars: None,
            pic_complete: None,
            pic_inter: None,
            pic_finale: None,
            pic_pause: None,
            damage_blend: 0.0,
            damage_color: [255, 0, 0],
            bonus_blend: 0.0,
            fade_clock: Tick72::default(),
            v_dmg_time: 0.0,
            v_dmg_roll: 0.0,
            v_dmg_pitch: 0.0,
            oldz: f32::NAN,
            centerprint: None,
            notify: ConNotify::default(),
            viewsize: render::VIEWSIZE_DEFAULT,
            crosshair: false,
            stepping: Stepping::Classic,
            lerpmove: LerpMove::Classic,
            glides: StepGlides::default(),
            lerpmodels: LerpModels::Classic,
            frame_lerps: FrameLerps::default(),
            sv_gravity: crate::server::ServerCvars::default().sv_gravity,
            trail_org: HashMap::new(),
            tracercount: 0,
            demonum: 0,
            show_scores: false,
            faceanimtime: 0.0,
            cl_items,
            item_gettime: [0.0; 32],
        }
    }
}

pub fn color_for_name(name: &str) -> [u8; 3] {
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

/// An angle as it crosses the wire in `svc_setangle`: `MSG_WriteAngle`
/// (`((int)f*256/360) & 255`) then `MSG_ReadAngle` (`MSG_ReadChar() *
/// (360.0/256)`) — whole degrees truncated, then 256 steps, signed.
pub fn net_angle(f: f32) -> f32 {
    let b = ((f as i32).wrapping_mul(256) / 360) & 255;
    (b as u8 as i8) as f32 * (360.0 / 256.0)
}

/// The view angles a freshly spawned client starts with, as `(yaw, pitch)`:
/// Host_Spawn_f (host_cmd.c) sends `svc_setangle` with the player entity's
/// `angles` right after PutClientInServer ("never send a roll angle"), so the
/// view faces the spot QuakeC's SelectSpawnPoint chose — `info_player_start`,
/// `info_player_start2` once a rune is held, or `testplayerstart`. Read after
/// the connect, before the settle frames.
pub fn spawn_view_angles(server: &Server, player: i32) -> (f32, f32) {
    let a = server.vm.ent_vec(player, server.vm.fo().angles);
    (net_angle(a[1]), clamp_pitch(net_angle(a[0])))
}

/// Shared tail of the walk builders ([`build_walk_map`](host_cmd::build_walk_map) / the savegame load
/// path): load the render-side assets (palette, gfx.wad, colormap, conchars,
/// plaque pics) from the pak and assemble a fresh [`Walk`] around an
/// already-built server. Every per-session field starts from its clean-slate
/// default (no particles/dlights/beams/notify, clock 0). Returns `None` only
/// when the palette is missing/unparseable (nothing could render).
#[allow(clippy::too_many_arguments)]
pub fn assemble_walk(
    pak: Pak,
    map: String,
    server: Server,
    player: i32,
    entry_parms: [f32; crate::server::NUM_SPAWN_PARMS],
    bsp: Bsp,
    yaw: f32,
    pitch: f32,
) -> Option<Walk> {
    let read = |n: &str| pak.read_file(n).ok().flatten();
    let palette = render::parse_palette(&read("gfx/palette.lmp")?)?;
    // The HUD pics live in gfx.wad; parse it once (None if absent/unparseable).
    let gfx_wad = read("gfx.wad").and_then(|b| crate::wad::Wad2::parse(b).ok());
    let colormap = read("gfx/colormap.lmp");
    let conchars = gfx_wad.as_ref().and_then(render::conchars_pic);
    // The intermission/finale plaques are pak `.lmp` pics (Draw_CachePic in the
    // C), loaded once like the menu pics; any absent one just doesn't draw.
    let lmp = |n: &str| -> Option<Qpic> { read(n).and_then(|b| Qpic::parse(&b).ok()) };
    let pic_complete = lmp("gfx/complete.lmp");
    let pic_inter = lmp("gfx/inter.lmp");
    let pic_finale = lmp("gfx/finale.lmp");
    let pic_pause = lmp("gfx/pause.lmp");
    let clock = server.time(); // cl.time = sv.time (see `Walk::clock`)
    // CL_ClearState + the signon's clientdata: what the player spawns with,
    // unflashed (`view::stamp_item_gettime`).
    let cl_items = cl_main::server_items(&server, player);
    let mut renderer = render::Renderer::new();
    renderer.begin_map(&bsp);
    Some(Walk {
        server,
        bsp,
        renderer,
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
        bonus_blend: 0.0,
        fade_clock: Tick72::default(),
        v_dmg_time: 0.0,
        v_dmg_roll: 0.0,
        v_dmg_pitch: 0.0,
        faceanimtime: 0.0,
        damage_count: 0.0,
        cl_items,
        item_gettime: [0.0; 32],
        oldz: f32::NAN,
        centerprint: None,
        notify: ConNotify::default(),
        viewsize: render::VIEWSIZE_DEFAULT,
        crosshair: false,
        stepping: Stepping::Classic,
        lerpmove: LerpMove::Classic,
        glides: StepGlides::default(),
        lerpmodels: LerpModels::Classic,
        frame_lerps: FrameLerps::default(),
        clock,
        host_time: 0.0,
        host_clock: 0.0,
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
        host_error: None,
        pic_complete,
        pic_inter,
        pic_finale,
        pic_pause,
    })
}

// ---------------------------------------------------------------------------
// The screen a frame draws, and what it hands back
// ---------------------------------------------------------------------------

/// The screen a client frame draws — vid.h's `viddef_t` as the platform set
/// the mode — and how the platform shows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vid {
    /// The mode's size in pixels (`vid.width` x `vid.height`).
    pub width: usize,
    pub height: usize,
    /// The width:height ratio the platform DISPLAYS the whole frame at (the
    /// browser page: 4:3, as DOS and Windows Quake's modes filled a 4:3
    /// monitor); with the mode's size it gives `vid.aspect`.
    pub display_aspect: f64,
    /// Exact perspective at every pixel of walls and liquids instead of id's
    /// 16-pixel spans (`D_DrawSpans16`): the web port's `wasm_exactpersp`
    /// extra, off in id's Quake.
    pub exact_perspective: bool,
    /// The port's video cvars (Hor+, views past id's largest mode): Classic
    /// in id's Quake.
    pub video: render::VideoCvars,
    /// id's `d_mipscale` / `d_mipcap` (`MipCvars::DEFAULT`, id's defaults).
    pub mip: render::MipCvars,
}

/// How the renderer draws the 3-D view `vrect` of the frame `vid` describes:
/// `vid.aspect` for that mode on its display (vid_win.c's `(h/w)*(320/240)`:
/// 0.8333 at every 16:10 mode shown at 4:3), which `R_ViewChanged` folds into
/// the projection so the world is not stretched by the display; where the
/// view sits on that screen (`D_Sky_uv_To_st` centres the sky on the screen);
/// the video cvars, and id's `d_mipscale`/`d_mipcap`; and the renderer extra,
/// off unless the platform switched it on.
pub fn render_options(vrect: &render::ViewRect, vid: &Vid) -> render::RenderOptions {
    render::RenderOptions {
        pixel_aspect: render::vid_aspect(vid.width, vid.height, vid.display_aspect),
        screen: Some(render::ScreenPlace { x: vrect.x, y: vrect.y, vid_w: vid.width, vid_h: vid.height }),
        exact_perspective: vid.exact_perspective,
        video: vid.video,
        mip: vid.mip,
    }
}

/// The `backtile` pic (`draw_backtile`, gfx.wad) for [`render::compose_view`],
/// fetched only when the 3-D view leaves part of the screen to tile-clear
/// (viewsize below 120). `None` when the view covers the whole frame or the
/// wad lacks it (then the border fills black).
pub fn backtile_for(
    vrect: &render::ViewRect,
    render_w: usize,
    render_h: usize,
    gfx_wad: Option<&crate::wad::Wad2>,
) -> Option<Qpic> {
    if vrect.w == render_w && vrect.h == render_h {
        return None;
    }
    gfx_wad.and_then(|g| g.qpic("backtile").ok())
}

/// One client frame, as the platform presents it: the finished screen, the
/// palette shift to present it through, and the frame's sound calls.
pub struct ClientFrame {
    /// The screen: the 3-D view at its rectangle, the backtile around it, the
    /// status bar or intermission overlay, the centerprint and notify lines.
    /// (The menu and the console are the host's, drawn over it.)
    pub image: render::Image,
    /// `cl.cshifts` in order — contents, damage, bonus, powerup — for
    /// `V_UpdatePalette`: the software renderer's palette shift tints the
    /// WHOLE screen, so the platform applies them after the menu and console.
    pub cshifts: Vec<([u8; 3], f32)>,
    /// What the frame said to the sound layer, in call order.
    pub sound: Vec<SoundCall>,
}

/// `S_Update` for the listener `listener` in `bsp` over a frame of `dt`: its
/// pose, and what `S_UpdateAmbientSounds` ramps the four automatic ambient
/// channels toward — the listener's leaf's `ambient_level[]` targets (water
/// wash / sky wind); `None` outside the world, where the C's `!l` branch
/// silences the channels without resetting the ramp. The C runs it from
/// `S_Update` in play and demo playback alike.
pub fn s_update(bsp: &Bsp, listener: Listener, dt: f32) -> SoundCall {
    let frametime = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
    let leaf_ambient = render::point_in_leaf(bsp, listener.pos)
        .and_then(|li| bsp.leafs.get(li))
        .map(|l| l.ambient_level);
    SoundCall::Update { listener, leaf_ambient, frametime }
}

// ---------------------------------------------------------------------------
// What the client hands the sound layer
// ---------------------------------------------------------------------------

/// The listener pose `S_Update` takes: the eye, and the forward and right unit
/// vectors of the view's yaw (level: panning needs only the horizontal plane).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Listener {
    pub pos: [f32; 3],
    pub forward: [f32; 3],
    pub right: [f32; 3],
}

impl Listener {
    /// All zero: no listener yet.
    pub const fn zero() -> Self {
        Listener { pos: [0.0; 3], forward: [0.0; 3], right: [0.0; 3] }
    }
}

/// One call the client makes into the sound layer — snd_dma.c's entry
/// points, and cd_audio.c's, which id's client called from the same places
/// (`CL_ParseServerMessage`) — recorded in call order for the platform to
/// carry out (the browser's program hands them to the engine's mixer,
/// [`crate::snd::Mixer`], and its CD player). A frame's calls
/// come back in its [`ClientFrame`]; a level load makes them into a caller's
/// `Vec`.
#[derive(Clone, Debug)]
pub enum SoundCall {
    /// `S_StartSound` for each event, in order. `view_entity` is the
    /// listener's own entity (`cl.viewentity`), whose sounds `SND_Spatialize`
    /// plays at full volume; within one call a later event on the same
    /// non-zero (entity, channel) overrides an earlier one (`SND_PickChannel`).
    Start { events: Vec<SoundEvent>, view_entity: i32 },
    /// `S_StopSound(entity, channel)` for each pair (`svc_stopsound`).
    Stop(Vec<(i32, i32)>),
    /// `S_StopAllSounds`: a level or mode change ends every sound, the
    /// placed loops and the ambient ramps included.
    StopAll,
    /// `S_StaticSound` for each of a level's placed loops (`ambientsound`,
    /// `svc_spawnstaticsound`).
    Static(Vec<StaticSound>),
    /// `S_Update`: the listener pose, and what `S_UpdateAmbientSounds` ramps
    /// the four ambient channels toward — the listener leaf's
    /// `ambient_level[]` (`None` outside the world) — over `frametime`.
    Update { listener: Listener, leaf_ambient: Option<[u8; NUM_AMBIENTS]>, frametime: f32 },
    /// A call into the CD player (`CDAudio_Play`, `_Pause`, `_Resume`:
    /// [`crate::cd_audio`]), which plays beside the mixer, not through it.
    Cd(CdCall),
}

// ---------------------------------------------------------------------------
// Frame timers
// ---------------------------------------------------------------------------

/// A phase of the host frame, in execution order, for a platform's frame
/// timers (quake-wasm's `--features bench` build, `web/bench.py`). The client
/// frames lap `Sim`, `Render3d`, `Post3d` and `Hud2d`; the host laps the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Input = 0,
    Sim,
    Render3d,
    Post3d,
    Hud2d,
    Menu,
    Console,
    Blend,
    Pack,
}

thread_local! {
    /// The frame timer [`lap`] calls, if a platform installed one.
    static LAP_HOOK: Cell<Option<fn(Phase)>> = const { Cell::new(None) };
}

/// Install (or clear) the frame timer [`lap`] hands each phase boundary to.
/// None is installed by default: the game never times itself.
pub fn set_lap_hook(hook: Option<fn(Phase)>) {
    LAP_HOOK.with(|c| c.set(hook));
}

/// A phase boundary: the time since the previous lap belongs to `phase`.
/// Calls the installed timer ([`set_lap_hook`]); without one, a no-op.
#[inline]
pub fn lap(phase: Phase) {
    if let Some(hook) = LAP_HOOK.with(Cell::get) {
        hook(phase);
    }
}

thread_local! {
    /// The hook [`view_hook`] runs the finished 3-D view through, if any.
    static VIEW_HOOK: Cell<Option<ViewHook>> = const { Cell::new(None) };
}

/// A harness's last word on the finished 3-D view of a live frame — the
/// screen and the view's rectangle on it — before the 2-D layer is drawn over
/// it (see [`set_view_hook`]).
pub type ViewHook = fn(&mut render::Image, render::ViewRect);

/// Install (or clear) the [`ViewHook`]: the 2-D oracle harness (quake-wasm's
/// `oracle_screen`) paints the view one palette index, as the C oracle's
/// `oracle_blank` fills `scr_vrect`, so a shot measures the 2-D layer alone.
/// None is installed by default.
pub fn set_view_hook(hook: Option<ViewHook>) {
    VIEW_HOOK.with(|c| c.set(hook));
}

/// The 3-D view at `vrect` of `screen` through the installed [`ViewHook`]
/// (unchanged without one).
#[inline]
pub fn view_hook(screen: &mut render::Image, vrect: render::ViewRect) {
    if let Some(hook) = VIEW_HOOK.with(Cell::get) {
        hook(screen, vrect);
    }
}
