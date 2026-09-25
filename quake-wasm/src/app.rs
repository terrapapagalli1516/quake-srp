//! The shell's state and its boots — the [`App`] (host-level state that
//! outlives a level: mode, menu, console, clocks, framebuffer, held keys)
//! and the two things it runs, the live [`Walk`] and the recorded
//! [`DemoPlay`] (client.h's `client_state_t`, one per mode); host.c's
//! one-time asset loads, the walk builders (`SV_SpawnServer` + the client
//! connect, as `map` runs them), `CL_PlayDemo_f`'s demo build, and the
//! `boot*` exports the page starts a mode with.

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

use crate::PAK;
use crate::console::ConNotify;
use crate::host::ShowFps;
use crate::cl_walk::net_angle;
use crate::input::{clamp_pitch, KeyMove};
use crate::snd_dma::{bump_sound_generation, queue_static_sounds, SND_QUEUE, STOP_SND_QUEUE};
use crate::vid::{DEFAULT_H, DEFAULT_W};

const WALK_MAP: &str = "maps/e1m1.bsp";
/// quake.rc's `startdemos demo1 demo2 demo3`: the attract loop's demos, played
/// in turn (`CL_NextDemo` on each demo's `svc_disconnect`), wrapping to the first.
pub(crate) const DEMOS: [&str; 3] = ["demo1.dem", "demo2.dem", "demo3.dem"];

/// Interactive walk state: a live server ticked every frame, rendered from the
/// player edict. Monster thinks advance their animation frames and move them,
/// and the sound queue surfaces the events they fire.
pub(crate) struct Walk {
    pub(crate) server: Server,
    /// A second copy of the map BSP for rendering (the server owns its own copy
    /// inside the world host).
    pub(crate) bsp: Bsp,
    pub(crate) palette: [[u8; 3]; 256],
    /// The parsed `gfx.wad` (sbar + digit pics) for the status-bar HUD overlay,
    /// or `None` if the archive lacked/could not parse it. Parsed once at boot so
    /// the per-frame HUD draw is allocation-light.
    pub(crate) gfx_wad: Option<quake_rs::wad::Wad2>,
    /// `gfx/colormap.lmp` — the 64x256 shade LUT for faithful colormap-indexed
    /// wall lighting (Quake never overbrights). `None` falls back to the linear
    /// brightness multiply. Loaded once at boot.
    pub(crate) colormap: Option<Vec<u8>>,
    /// The `conchars` font (extracted once from `gfx_wad`) for the on-screen
    /// message overlay — centerprint (centered) + notify lines (top-left).
    pub(crate) conchars: Option<Qpic>,
    /// The archive, kept open so sound samples load on demand as events fire.
    pub(crate) pak: Pak,
    /// Parsed alias models keyed by in-pak name (`None` = absent/unparseable).
    pub(crate) model_cache: HashMap<String, Option<Mdl>>,
    /// Parsed *external brush* models keyed by in-pak name (`None` =
    /// absent/unparseable). These are Quake's standalone `maps/b_*.bsp` item
    /// boxes (explosive box, ammo/health boxes) that items `setmodel()` to at
    /// runtime. Cached like `model_cache` so a box parses once and backs every
    /// instance of that item; rendered via [`render::ExternalBModel`].
    pub(crate) bmodel_cache: HashMap<String, Option<Bsp>>,
    /// Parsed sprite models (`.spr`) keyed by name, cached like `model_cache` so a
    /// sprite (the explosion flash, bubbles) parses once and backs every instance.
    pub(crate) sprite_cache: HashMap<String, Option<quake_rs::spr::Sprite>>,
    /// Per-entity previous render origin, keyed by edict index — the source point
    /// for R_RocketTrail (rockets/grenades/gibs trail from their old origin to the
    /// new one each frame). Defaults to the current origin the first time an
    /// entity is seen, so there's no spurious trail on spawn.
    pub(crate) trail_org: HashMap<i32, [f32; 3]>,
    /// Quake's `tracercount` (CL_RelinkEntities `static int`): alternates the
    /// tracer-trail offset direction; threaded across `spawn_rocket_trail` calls.
    pub(crate) tracercount: u32,
    /// The world map's in-pak path (e.g. `maps/e1m1.bsp`). An entity whose
    /// `model` equals this is the worldspawn brush — never loaded as an external
    /// box (it is already drawn as the world).
    pub(crate) map_name: String,
    /// The spawn parms captured when this level was ENTERED (the alive player's
    /// inventory at level start). A single-player respawn (`localcmd("restart")`)
    /// reloads the current level with THESE, since a dead player's state is empty.
    pub(crate) entry_parms: [f32; quake_rs::server::NUM_SPAWN_PARMS],
    pub(crate) player: i32,
    pub(crate) yaw: f32,
    pub(crate) pitch: f32,
    pub(crate) in_fwd: f32,
    pub(crate) in_side: f32,
    pub(crate) in_attack: bool,
    /// Whether the jump key is held (UserCmd button bit 1 -> the player's
    /// `button2`, which the QuakeC PlayerJump reads to leap when on the ground).
    pub(crate) in_jump: bool,
    /// Whether the swim-down key (`c`) is held: drives `UserCmd.upmove` negative,
    /// which `SV_WaterMove` reads to sink. Ignored out of water (the WALK air
    /// move zeroes the vertical wish). Swim-UP reuses `in_jump` (Space) the same
    /// way — in water it pushes up, on land it just jumps.
    pub(crate) in_down: bool,
    /// A one-shot impulse (weapon switch etc.) queued by `set_impulse`, applied
    /// to the next `step_walk` UserCmd then cleared — matching how Quake's
    /// `impulse` console command fires once. 0 means "no impulse this frame".
    pub(crate) next_impulse: i32,
    /// This frame's bindings-derived keyboard input (CL_BaseMove/CL_AdjustAngles
    /// over the page-held keys), refreshed by `step` before `step_walk` runs.
    pub(crate) key_move: KeyMove,
    /// The `viewsize` cvar this frame (the Options "Screen size" slider),
    /// refreshed by `step` from the menu before stepping, like `key_move`:
    /// [`render::calc_refdef`] turns it into the 3-D view rectangle and how
    /// much status bar shows.
    pub(crate) viewsize: f32,
    /// Accumulated mouse-strafe sidemove units (in_win.c IN_MouseMove's
    /// `cmd->sidemove += m_side.value * mouse_x` when lookstrafe / +strafe route
    /// mouse X away from yaw). Drained into the next UserCmd then cleared.
    pub(crate) mouse_side: f32,
    /// Accumulated mouse forwardmove units (IN_MouseMove's else branch:
    /// `cmd->forwardmove -= m_forward.value * mouse_y` while +strafe holds mouse
    /// Y out of the pitch path). Drained like `mouse_side`.
    pub(crate) mouse_fwd: f32,
    /// Pitch drift active (view.c `!cl.nodrift`): centerview or a lookspring
    /// pointer-unlock started it; the view re-levels at `pitch_vel` deg/sec until
    /// it reaches 0 or mouse/keyboard look stops it (V_StopPitchDrift).
    pub(crate) pitch_drift: bool,
    /// `cl.pitchvel` — the drift rate, seeded with [`V_CENTERSPEED`] and
    /// accelerated by it each second while drifting (V_DriftPitch).
    pub(crate) pitch_vel: f32,
    /// Full-screen damage-flash intensity (Quake's `CSHIFT_DAMAGE` percent,
    /// 0..150): bumped by each svc_damage (V_ParseDamage) and faded each frame.
    pub(crate) damage_blend: f32,
    /// The damage-flash tint colour (`V_ParseDamage` picks (200,100,100) when armour
    /// absorbs most, (220,50,50) for armour-only, (255,0,0) for pure blood).
    pub(crate) damage_color: [u8; 3],
    /// `cl.cshifts[CSHIFT_BONUS].percent`: the gold pickup flash a stuffed
    /// `bf` sets to 50 (V_BonusFlash_f), dropped `dt*100` per frame.
    pub(crate) bonus_blend: f32,
    /// `v_dmg_time` / `v_dmg_roll` / `v_dmg_pitch` (view.c): the directional
    /// view kick of the last svc_damage, decaying over `v_kicktime`.
    pub(crate) v_dmg_time: f32,
    pub(crate) v_dmg_roll: f32,
    pub(crate) v_dmg_pitch: f32,
    /// `cl.faceanimtime` (V_ParseDamage: `cl.time + 0.2`, on the server clock
    /// like the HUD's `time`): the status bar shows the pain face until then.
    pub(crate) faceanimtime: f32,
    /// `cl.items` as last received and `cl.item_gettime[]` (CL_ParseClientdata,
    /// server clock): the new-weapon icon flash. Zeroed with the level
    /// (CL_ClearState), so a level start flashes what the player carries.
    pub(crate) cl_items: i32,
    pub(crate) item_gettime: [f32; 32],
    /// Stair-step view smoothing accumulator (`view.c` V_CalcRefdef `oldz`): the eye
    /// Z lags the player Z by up to 12 units while climbing so stairs glide instead
    /// of jolting. NaN until the first frame establishes it.
    pub(crate) oldz: f32,
    /// Current centered message (`centerprint`) + the clock time it expires at
    /// (Quake's `scr_centertime` ~2s); replaced by the next centerprint. Drawn
    /// centered over the view.
    pub(crate) centerprint: Option<(String, f32)>,
    /// The top-left notify lines (`bprint`/`sprint` through Con_Print, shown by
    /// Con_DrawNotify), on the host clock.
    pub(crate) notify: ConNotify,
    /// `cl.time`: accumulated game time (seconds), advanced by `dt` each
    /// `step_walk` that runs the server. On a local server the C's
    /// `CL_LerpPoint` snaps `cl.time` to the server's message time, so it stops
    /// with the server while single player is paused behind the menu/console.
    /// Drives the animated surfaces, light styles, particles, dlight decay, the
    /// rotating pickups, the bob and the intermission sway.
    pub(crate) clock: f32,
    /// Host time (seconds): advanced by every frame's `dt`, paused or not. The
    /// notify lines (Con_DrawNotify ages them in `realtime`) and the centerprint
    /// (SCR_CheckDrawCenterString counts `scr_centertime_off` down by
    /// `host_frametime`) expire on this clock, so they keep timing out behind the
    /// menu as in the C.
    pub(crate) host_time: f32,
    /// Live engine particles (the `particle()` builtin's effect). Bursts the
    /// QuakeC fires each frame are drained into this pool, aged under gravity,
    /// and drawn into the scene sharing its z-buffer.
    pub(crate) particles: ParticleSystem,
    /// Deterministic RNG for particle spawns (no `rand` crate; std-only).
    pub(crate) prng: Lcg,
    /// Live dynamic lights (explosions, muzzle flashes, EF_* lights). Allocated
    /// each frame from the drained temp entities + the server's entity_dlights,
    /// decayed under `advance`, and passed to the renderer to light the walls.
    pub(crate) dlights: DynamicLights,
    /// The beam temp-entity slots (`cl_beams`): lightning bolts the drained
    /// `TE_LIGHTNING1/2/3` / `TE_BEAM` events refresh ([`Beams::parse_beam`])
    /// and [`step_walk`] expands into bolt-model instances each frame
    /// (`CL_UpdateTEnts`). Cleared on changelevel/restart (`CL_ClearState`).
    pub(crate) beams: Beams,
    /// Reused per-frame scratch for the expanded beam pieces (no per-frame
    /// allocation on the common no-beam frames; `Beams::update` clears it).
    pub(crate) beam_scratch: Vec<BeamSegment>,
    /// `cl.intermission` (client.h): 0 = playing, 1 = the level-complete stats
    /// overlay (svc_intermission), 2 = the episode finale text + plaque
    /// (svc_finale), 3 = cutscene text only (svc_cutscene). While non-zero the
    /// view is the QC-placed intermission camera (V_CalcIntermissionRefdef): no
    /// bob/roll/punch, no viewmodel, no status bar.
    pub(crate) intermission: u8,
    /// `cl.completed_time` — the clock latched when the intermission started
    /// (the overlay's minutes:seconds completion time).
    pub(crate) completed_time: f32,
    /// The `svc_finale`/`svc_cutscene` text (`SCR_CenterPrint`'d in the C),
    /// revealed at `scr_printspeed` (8) chars/sec from `finale_start`.
    pub(crate) finale_text: String,
    /// `scr_centertime_start` — the clock when the finale text began revealing.
    pub(crate) finale_start: f32,
    /// `svc_sellscreen` arrived this frame: the C ran `Cmd_ExecuteString("help")`,
    /// i.e. popped the Help/Ordering menu — the `step` dispatcher (which owns the
    /// menu) takes this flag and opens it.
    pub(crate) pending_sellscreen: bool,
    /// `gfx/complete.lmp` — the "Level Complete" banner (Sbar_IntermissionOverlay).
    pub(crate) pic_complete: Option<Qpic>,
    /// `gfx/inter.lmp` — the Time/Secrets/Kills intermission plaque.
    pub(crate) pic_inter: Option<Qpic>,
    /// `gfx/finale.lmp` — the finale plaque (Sbar_FinaleOverlay).
    pub(crate) pic_finale: Option<Qpic>,
}

/// Recorded-demo playback state.
pub(crate) struct DemoPlay {
    pub(crate) bsp: Bsp,
    pub(crate) palette: [[u8; 3]; 256],
    pub(crate) demo: Demo,
    /// The archive, kept open so the recorded `svc_sound` one-shots can load
    /// their WAV bytes on demand — the demo's audio runs through the SAME
    /// `queue_sounds` path live play uses.
    pub(crate) pak: Pak,
    /// Parsed model per precache index (None for non-`.mdl` / missing).
    pub(crate) models: Vec<Option<Mdl>>,
    /// Parsed sprite per precache index (None for non-`.spr` / missing); the boot
    /// demo's explosion flashes (s_explod.spr) render from these.
    pub(crate) sprites: Vec<Option<quake_rs::spr::Sprite>>,
    /// `gfx/colormap.lmp` — the 64-row shade LUT. The demo path must thread it like
    /// the live walk does, else world surfaces render overbright (linear fallback)
    /// instead of through id's no-overbright colormap shading.
    pub(crate) colormap: Option<Vec<u8>>,
    pub(crate) colors: Vec<[u8; 3]>,
    pub(crate) elapsed: f32,
    pub(crate) idx: usize,
    /// Live particles replayed from the recorded `svc_particle` / temp-entity
    /// stream: each frame's effects are spawned ONCE when playback advances onto
    /// it, then the pool is aged under gravity and drawn into the scene (sharing
    /// its z-buffer) — so the demo shows blood, gunshot puffs and explosions just
    /// like [`step_walk`] does for live play.
    pub(crate) particles: ParticleSystem,
    /// Deterministic RNG for the demo's particle spawns (std-only, like Walk).
    pub(crate) prng: Lcg,
    /// The frame index whose effects were last spawned, so a frame rendered for
    /// several steps spawns its bursts only on the step that ADVANCES onto it
    /// (never re-spawning while it lingers). `usize::MAX` = "none spawned yet".
    pub(crate) last_spawned_idx: usize,
    /// The beam temp-entity slots (`cl_beams`) replayed from the recorded
    /// `TE_LIGHTNING1/2/3` / `TE_BEAM` stream; expanded into bolt-model
    /// instances each frame like the live walk. Cleared on the demo loop wrap.
    pub(crate) beams: Beams,
    /// Reused per-frame scratch for the expanded beam pieces.
    pub(crate) beam_scratch: Vec<BeamSegment>,
    /// `gfx.wad` (the big digit pics) for a recorded intermission's stats
    /// overlay; `None` degrades to no overlay, never a panic.
    pub(crate) gfx_wad: Option<quake_rs::wad::Wad2>,
    /// The conchars font for a recorded finale's revealed center string.
    pub(crate) conchars: Option<Qpic>,
    /// `gfx/complete.lmp` / `gfx/inter.lmp` / `gfx/finale.lmp` — the plaques a
    /// recorded intermission/finale frame draws (Sbar_Intermission/FinaleOverlay).
    pub(crate) pic_complete: Option<Qpic>,
    pub(crate) pic_inter: Option<Qpic>,
    pub(crate) pic_finale: Option<Qpic>,
    /// `cl.cshifts[CSHIFT_DAMAGE].percent` for the recorded POV: bumped by each
    /// recorded `svc_damage` (V_ParseDamage: `+= 3*count`, clamped 0..150) and
    /// faded `dt*150` per rendered frame (V_UpdatePalette), exactly like the
    /// live walk's damage flash.
    pub(crate) damage_blend: f32,
    /// The damage tint V_ParseDamage picked (armour-dominant pink / armour
    /// orange-red / pure-blood red).
    pub(crate) damage_color: [u8; 3],
    /// `cl.cshifts[CSHIFT_BONUS].percent` for the recorded POV: a recorded
    /// `svc_stufftext "bf"` sets it to 50 (V_BonusFlash_f), faded `dt*100`.
    pub(crate) bonus_blend: f32,
    /// `v_dmg_time` / `v_dmg_roll` / `v_dmg_pitch` (view.c): the directional
    /// view kick a recorded svc_damage applies, decaying over `v_kicktime`.
    pub(crate) v_dmg_time: f32,
    pub(crate) v_dmg_roll: f32,
    pub(crate) v_dmg_pitch: f32,
    /// Stair-step smoothing accumulator (V_CalcRefdef `oldz`) for the recorded
    /// view entity; NaN until the first frame establishes it.
    pub(crate) oldz: f32,
    /// Current centerprint + expiry (recorded `svc_centerprint`, scr_centertime
    /// ~2 s on the demo's recorded frame clock).
    pub(crate) centerprint: Option<(String, f32)>,
    /// The notify lines (recorded `svc_print` through Con_Print), on the
    /// recorded clock.
    pub(crate) notify: ConNotify,
    /// The `viewsize` cvar this frame (the Options "Screen size" slider),
    /// refreshed by `step` from the menu before stepping, like `key_move`:
    /// [`render::calc_refdef`] turns it into the 3-D view rectangle and how
    /// much status bar shows.
    pub(crate) viewsize: f32,
    /// Each relinked entity's origin as last rendered (CL_RelinkEntities'
    /// `oldorg`), keyed by entity number, for the model-flag trails; an entity
    /// missing from a frame is forgotten (its next sighting is a forcelink).
    pub(crate) trail_org: HashMap<i32, [f32; 3]>,
    /// R_RocketTrail's `static int tracercount` for the demo's tracer trails.
    pub(crate) tracercount: u32,
    /// Which of [`DEMOS`] this is (the next one follows it, CL_NextDemo).
    pub(crate) demonum: usize,
    /// `sb_showscores` (`+showscores`, Tab held): Sbar_Draw shows the solo
    /// scoreboard during playback too. Refreshed by `step` like `viewsize`.
    pub(crate) show_scores: bool,
    /// `cl.faceanimtime` (V_ParseDamage: `cl.time + 0.2`): the status bar
    /// shows the pain face until then.
    pub(crate) faceanimtime: f32,
    /// `cl.items` as last shown and `cl.item_gettime[]` on the recorded clock
    /// (CL_ParseClientdata): the new-weapon icon flash. Zeroed at playback
    /// start and on the loop wrap (CL_ClearState).
    pub(crate) cl_items: i32,
    pub(crate) item_gettime: [f32; 32],
}

impl DemoPlay {
    /// The last frame has been shown: the recording is over (id's demos end
    /// with `svc_disconnect`, which the parser stops at).
    pub(crate) fn at_end(&self) -> bool {
        self.idx + 1 >= self.demo.frames.len()
    }

    /// A playback of `demo` over `bsp` at its first frame: no models, sprites,
    /// colormap or overlay pics yet (the caller loads what it has), and every
    /// per-playback field — clocks, particles, beams, view shifts, messages —
    /// at its clean-slate default.
    pub(crate) fn new(pak: Pak, bsp: Bsp, palette: [[u8; 3]; 256], demo: Demo) -> DemoPlay {
        DemoPlay {
            bsp,
            palette,
            demo,
            pak,
            models: Vec::new(),
            sprites: Vec::new(),
            colormap: None,
            colors: Vec::new(),
            elapsed: 0.0,
            idx: 0,
            particles: ParticleSystem::new(),
            prng: Lcg::new(0x9E37_79B9),
            last_spawned_idx: usize::MAX,
            beams: Beams::new(),
            beam_scratch: Vec::new(),
            gfx_wad: None,
            conchars: None,
            pic_complete: None,
            pic_inter: None,
            pic_finale: None,
            damage_blend: 0.0,
            damage_color: [255, 0, 0],
            bonus_blend: 0.0,
            v_dmg_time: 0.0,
            v_dmg_roll: 0.0,
            v_dmg_pitch: 0.0,
            oldz: f32::NAN,
            centerprint: None,
            notify: ConNotify::default(),
            viewsize: render::VIEWSIZE_DEFAULT,
            trail_org: HashMap::new(),
            tracercount: 0,
            demonum: 0,
            show_scores: false,
            faceanimtime: 0.0,
            cl_items: 0,
            item_gettime: [0.0; 32],
        }
    }
}

pub(crate) struct App {
    pub(crate) walk: Option<Walk>,
    pub(crate) demo: Option<DemoPlay>,
    /// 0 = walk, 1 = demo.
    pub(crate) mode: u8,
    /// The main-menu engine. Lives at the App level (mode-independent) so it can
    /// overlay WHATEVER is playing — the walk OR the attract demo. Quake boots
    /// INTO the menu over the playing attract demo; while `menu.visible`,
    /// gameplay input is gated (the world still idles) and the `step` dispatcher
    /// overlays `draw_menu` on the finished frame.
    pub(crate) menu: Menu,
    /// The menu's plaque/title/list/cursor pics, loaded once on first boot from
    /// the pak (they are mode-independent). `None` until `ensure_menu_assets`
    /// runs once.
    pub(crate) menu_pics: MenuPics,
    /// The 128x128 `conchars` font atlas (wrapped as a Qpic) for `draw_string`,
    /// or `None` if `gfx.wad`/conchars were absent. Loaded alongside `menu_pics`.
    pub(crate) conchars: Option<Qpic>,
    /// Whether the menu assets (`menu_pics` + `conchars`) have been loaded yet.
    /// `ensure_menu_assets` loads them once on first boot; subsequent boots reuse
    /// them (they never change).
    pub(crate) menu_loaded: bool,
    /// The drop-down console (toggled with `~`). Mode-independent like the menu:
    /// it overlays whatever is playing and, while open, owns the keyboard. Its
    /// commands act on the live [`Walk`].
    pub(crate) console: Console,
    /// The `gfx/conback.lmp` console background (a 320x200 QPIC), loaded once
    /// alongside the menu assets. `None` if the pak lacked it — `draw_console`
    /// then falls back to a dark fill.
    pub(crate) conback: Option<Qpic>,
    /// `host_time` (host.c): the accumulated CLAMPED frame time (seconds) —
    /// `step`'s `dt` after Host_FilterTime's 0.1 s cap — advanced every `step`
    /// regardless of mode. Drives the menudot spinner (`(int)(host_time*10) % 6`
    /// in `M_Main_Draw` and friends), which keeps turning over a frozen frame.
    pub(crate) clock: f32,
    /// `realtime` (host.c): the UNCLAMPED wall clock (seconds) — `step`'s raw
    /// `dt` summed, before Host_FilterTime caps the frame time. Drives every
    /// flashing cursor the C times on `realtime`: the menu cursors
    /// (`12 + ((int)(realtime*4)&1)`) and the console input cursor
    /// (`Con_DrawInput`, `con_cursorspeed` 4).
    pub(crate) realtime: f64,
    /// `oldrealtime` (host.c): `realtime` when the last host frame ran —
    /// [`host_filter_time`]'s gate measures the time since then.
    pub(crate) oldrealtime: f64,
    /// Current render resolution (runtime; defaults to [`DEFAULT_W`] x
    /// [`DEFAULT_H`]). The scene renders at this size and the framebuffer is
    /// `render_w * render_h * 4` RGBA bytes, reallocated whenever it changes.
    pub(crate) render_w: usize,
    pub(crate) render_h: usize,
    pub(crate) fb: Vec<u8>, // RGBA, render_w*render_h*4
    /// The page-held key states by Quake keynum (keys.c `keydown[256]`), fed by
    /// [`key_down`]/[`key_up`]. Mode-independent (held keys survive a level
    /// change) and consulted through the menu's binding table each `step`.
    pub(crate) keys_held: [bool; 256],
    /// The gamma the current [`App::gamma_table`] was built for (V_CheckGamma's
    /// `oldgammavalue`): the table rebuilds only when the menu's `v_gamma`
    /// actually changes.
    pub(crate) gamma_value: f32,
    /// The 256-entry gamma LUT (view.c `gammatable`), applied where the finished
    /// frame is packed into the presented RGBA framebuffer — the port's
    /// hardware-palette boundary (`VID_ShiftPalette`). Identity at gamma 1.0,
    /// where the pack skips it entirely (byte-exact default).
    pub(crate) gamma_table: [u8; 256],
    /// The presented-frame counter behind the `wasm_showfps` extra.
    pub(crate) show_fps: ShowFps,
}

impl App {
    /// Resize the framebuffer to the (already-clamped) `(w, h)`, reallocating only
    /// when the size actually changes. The new buffer is zero-filled; the next
    /// `step` paints it.
    pub(crate) fn set_render_size(&mut self, w: usize, h: usize) {
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
    pub(crate) fn active_palette(&self) -> Option<&[[u8; 3]; 256]> {
        if self.mode == 1 {
            self.demo.as_ref().map(|d| &d.palette)
        } else {
            self.walk.as_ref().map(|w| &w.palette)
        }
    }
}

thread_local! {
    pub(crate) static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

pub(crate) fn color_for_name(name: &str) -> [u8; 3] {
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

/// The view angles a freshly spawned client starts with, as `(yaw, pitch)`:
/// Host_Spawn_f (host_cmd.c) sends `svc_setangle` with the player entity's
/// `angles` right after PutClientInServer ("never send a roll angle"), so the
/// view faces the spot QuakeC's SelectSpawnPoint chose — `info_player_start`,
/// `info_player_start2` once a rune is held, or `testplayerstart`. Read after
/// the connect, before the settle frames.
pub(crate) fn spawn_view_angles(server: &Server, player: i32) -> (f32, f32) {
    let a = server.vm.ent_get_vector(player, "angles");
    (net_angle(a[1]), clamp_pitch(net_angle(a[0])))
}

#[cfg(test)]
pub(crate) fn player_start(ents: &str) -> Option<([f32; 3], f32)> {
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
pub(crate) fn pak() -> Option<quake_rs::pak::Pak> {
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

pub(crate) fn build_walk() -> Option<Walk> {
    build_walk_map(WALK_MAP)
}

/// Shared tail of the walk builders ([`build_walk_map`] / the savegame load
/// path): load the render-side assets (palette, gfx.wad, colormap, conchars,
/// plaque pics) from the pak and assemble a fresh [`Walk`] around an
/// already-built server. Every per-session field starts from its clean-slate
/// default (no particles/dlights/beams/notify, clock 0). Returns `None` only
/// when the palette is missing/unparseable (nothing could render).
#[allow(clippy::too_many_arguments)]
pub(crate) fn assemble_walk(
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
        bonus_blend: 0.0,
        v_dmg_time: 0.0,
        v_dmg_roll: 0.0,
        v_dmg_pitch: 0.0,
        faceanimtime: 0.0,
        cl_items: 0,
        item_gettime: [0.0; 32],
        oldz: f32::NAN,
        centerprint: None,
        notify: ConNotify::default(),
        viewsize: render::VIEWSIZE_DEFAULT,
        clock: 0.0,
        host_time: 0.0,
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
pub(crate) fn build_walk_map(map: &str) -> Option<Walk> {
    let pak = pak()?;
    let read = |n: &str| pak.read_file(n).ok().flatten();
    let bsp = Bsp::parse(&read(map)?).ok()?;
    let bsp_sim = Bsp::parse(&read(map)?).ok()?;
    let progs = Progs::parse(&read("progs.dat")?).ok()?;

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
    let (yaw, pitch) = spawn_view_angles(&server, player);
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
    let _ = quake_rs::builtins::take_stufftext();

    assemble_walk(pak, map.to_string(), server, player, entry_parms, bsp, yaw, pitch)
}

/// The first attract demo (`demo1`).
pub(crate) fn build_demo() -> Option<DemoPlay> {
    build_demo_n(0)
}

/// `playdemo` of [`DEMOS`]`[demonum % 3]`.
pub(crate) fn build_demo_n(demonum: usize) -> Option<DemoPlay> {
    let demonum = demonum % DEMOS.len();
    let pak = pak()?;
    let read = |n: &str| pak.read_file(n).ok().flatten();
    let demo_bytes = read(DEMOS[demonum])?;
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

pub(crate) fn ensure_app(f: impl FnOnce(&mut App)) {
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
                show_fps: ShowFps::default(),
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

/// Start recorded-demo playback at demo1.dem (e1m3); demo2 and demo3 follow
/// (quake.rc's startdemos cycle, see [`DEMOS`]). Returns 1 on success.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cl_demo::step_demo;
    use crate::host::step;
    use crate::input::{key_down, key_up};
    use crate::menu::{
        menu_bind_key, menu_cancel, menu_down, menu_right, menu_select, menu_up, menu_visible,
    };
    use crate::test_util::*;

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
    fn attract_loop_cycles_demo1_demo2_demo3() {
        // quake.rc `startdemos demo1 demo2 demo3`: each demo's svc_disconnect
        // runs CL_NextDemo, so the attract loop plays the three in turn and
        // wraps — not demo1 forever.
        assert_eq!(boot_attract(), 1);
        let demo = || {
            APP.with(|c| {
                let b = c.borrow();
                let d = b.as_ref().unwrap().demo.as_ref().unwrap();
                (d.demonum, d.demo.map_name().unwrap_or("").to_string(), d.idx)
            })
        };
        let to_end = || {
            APP.with(|c| {
                let mut b = c.borrow_mut();
                let d = b.as_mut().unwrap().demo.as_mut().unwrap();
                d.idx = d.demo.frames.len() - 1; // the last frame has been shown
            })
        };
        let (n0, map0, _) = demo();
        assert_eq!(n0, 0);
        let mut maps = vec![map0];
        for want in [1, 2, 0] {
            to_end();
            step(0.05);
            let (n, map, idx) = demo();
            assert_eq!(n, want, "the next demo in startdemos order");
            assert!(idx < 10, "it plays from its start (frame {idx})");
            maps.push(map);
        }
        assert_eq!(maps[0], maps[3], "demo1 again after demo3");
        assert!(maps[0] != maps[1] && maps[1] != maps[2], "three different recordings: {maps:?}");
        assert_eq!(menu_visible(), 1, "the menu stays up over the loop");
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
            let (plain, _) = step_demo(d, 0.0001, false, w, h);
            let (mut withm, _) = step_demo(d, 0.0001, false, w, h);
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
}
