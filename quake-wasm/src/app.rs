//! The shell's state and its boots — the [`App`] (host-level state that
//! outlives a level: mode, menu, console, clocks, framebuffer, held keys)
//! around the client it runs, the live [`Walk`] or the recorded [`DemoPlay`]
//! ([`quake_rs::client`]); host.c's one-time asset loads, the client's level
//! loads with their sound calls carried out, and the boots (quake.rc's
//! attract loop at startup; the page's walk and demo buttons).

use std::cell::RefCell;

use quake_rs::client::cl_demo::{TimeDemoClock, MAX_DEMOS};
use quake_rs::client::{cl_demo, host_cmd};
use quake_rs::pak::Pak;
use quake_rs::render::{self, build_gamma_table, Console, Menu, MenuPics};
use quake_rs::wad::Qpic;

use crate::common::pak;
use crate::host::ShowFps;
use crate::snd_dma::{self, SND_QUEUE, STOP_SND_QUEUE};
use crate::vid::{DEFAULT_H, DEFAULT_W};

pub(crate) use quake_rs::client::{DemoPlay, Walk};

const WALK_MAP: &str = "maps/e1m1.bsp";

/// quake.rc's `startdemos demo1 demo2 demo3`: the attract loop.
pub(crate) const QUAKE_RC_DEMOS: [&str; 3] = ["demo1", "demo2", "demo3"];

/// `cls` (client.h `client_static_t`), its demo half: the `startdemos` loop
/// and `timedemo`'s bookkeeping. Host state: it outlives every demo.
#[derive(Debug, Default)]
pub(crate) struct Cls {
    /// `cls.demos`: the loop `startdemos` set, played in turn by
    /// `CL_NextDemo`. A shorter `startdemos` leaves the later slots as they
    /// were, as the C's strncpy does.
    pub(crate) demos: [String; MAX_DEMOS],
    /// `cls.demonum`: the loop's next demo; -1 = don't play demos (a game
    /// started, a `playdemo` failed, or `startdemos` found something already
    /// running). 0 at start, like the C's zeroed `cls`.
    pub(crate) demonum: i32,
    /// `cls.timedemo`: the demo plays one message a host frame, uncapped.
    pub(crate) timedemo: bool,
    /// `cls.td_startframe` / `cls.td_starttime`.
    pub(crate) td: TimeDemoClock,
}

pub(crate) struct App {
    pub(crate) walk: Option<Walk>,
    pub(crate) demo: Option<DemoPlay>,
    /// 0 = walk, 1 = demo.
    pub(crate) mode: u8,
    /// The main-menu engine. Lives at the App level (mode-independent) so it can
    /// overlay WHATEVER is playing — the walk OR the attract demo (any key
    /// during demo playback brings it up, as in Quake); while `menu.visible`,
    /// gameplay input is gated (single player pauses) and the `step`
    /// dispatcher overlays `draw_menu` on the finished frame.
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
    /// `host_framecount` (host.c): host frames completed — `step` calls that
    /// ran a frame. `timedemo` counts its frames on it.
    pub(crate) host_framecount: i64,
    /// `cls`'s demo loop and timedemo state.
    pub(crate) cls: Cls,
    /// `cls.state == ca_disconnected` after a disconnect (`stopdemo`, a demo
    /// ending outside the loop, a `playdemo` that could not open its file):
    /// nothing plays, and the console covers the screen (`con_forcedup`).
    /// Until the first boot the App is not id's startup screen, so this
    /// starts false.
    pub(crate) disconnected: bool,
    /// `gfx/palette.lmp`, for what is drawn with no level loaded (the
    /// disconnected screen's console and menu). Loaded with the menu assets.
    pub(crate) palette: Option<[[u8; 3]; 256]>,
    /// keys.c `key_repeats[256]`: key downs since each key's last up; a
    /// second down is the keyboard's autorepeat, which `Key_Event` ignores
    /// (Backspace and Pause aside).
    pub(crate) key_repeats: [u8; 256],
    /// keys.c `shift_down`: Shift is held, so a key types its `keyshift[]`.
    pub(crate) shift_down: bool,
    /// menu.c `m_save_demonum`: `cls.demonum` as the menu came up from
    /// outside it (`M_Menu_Main_f` switches the demo loop off while the menu
    /// is up; `M_Main_Key`'s Escape puts it back). 0 at start, a C static.
    pub(crate) m_save_demonum: i32,
    /// How many threads draw the 3-D view (the `r_threads` cvar; `Auto` by
    /// default): resolved each frame against [`App::hw_threads`] and handed
    /// to whichever game's renderer draws it (`host::step`). The pixels are
    /// the same for any count.
    pub(crate) render_threads: render::Threads,
    /// The threads the host offers the program: the page's pool of thread
    /// workers plus the program's own (`-hwthreads`, from `wasi.js`), else
    /// `std::thread::available_parallelism`; 1 without threads.
    pub(crate) hw_threads: usize,
    /// The port's video cvars (Hor+, views past id's 1280x1024) the frames
    /// are drawn with: Classic, id's, until a caller sets them
    /// (`set_video`); they also set how large a mode `set_resolution` takes.
    pub(crate) video: render::VideoCvars,
}

/// keys.c's `key_dest`: who gets the keyboard. The port keeps it as the menu's
/// and the console's open flags ([`App::key_dest`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyDest {
    /// `key_game`: keys go to their bindings (with nothing playing, the
    /// console keys type into the forced-up console).
    Game,
    /// `key_console`: the console is down.
    Console,
    /// `key_menu`: the menu is up.
    Menu,
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
            self.palette =
                pak.read_file("gfx/palette.lmp").ok().flatten().and_then(|b| render::parse_palette(&b));
        }
    }

    /// Whether typing goes to the console: it is open (`key_dest ==
    /// key_console`), or forced up while disconnected with no menu over it.
    pub(crate) fn console_has_keys(&self) -> bool {
        self.console.open || (self.disconnected && !self.menu.visible)
    }

    /// `key_dest`: the console while it is down, else the menu while it is
    /// up, else the game. (The two are never both open by a key: the console
    /// key goes to the menu's own keys while the menu is up.)
    pub(crate) fn key_dest(&self) -> KeyDest {
        if self.console.open {
            KeyDest::Console
        } else if self.menu.visible {
            KeyDest::Menu
        } else {
            KeyDest::Game
        }
    }

    /// `M_Menu_Main_f` (menu.c): the main menu opens on its kept cursor
    /// (`key_dest = key_menu`), and — coming from outside the menu — the demo
    /// loop stops while it is up (`m_save_demonum = cls.demonum; cls.demonum
    /// = -1`): the demo playing goes on to its end, and then the client
    /// disconnects instead of playing the next.
    pub(crate) fn m_menu_main(&mut self) {
        if !self.menu.visible {
            self.m_save_demonum = self.cls.demonum;
            self.cls.demonum = -1;
        }
        self.console.open = false;
        self.menu.open();
    }

    /// `M_Menu_Help_f` (menu.c; the `help` command and `svc_sellscreen`):
    /// the Help/Ordering screen on its first page, the menu taking the
    /// keyboard (`key_dest = key_menu`). The demo loop is untouched.
    pub(crate) fn m_menu_help(&mut self) {
        self.console.open = false;
        self.menu.open_help();
    }

    /// `M_ToggleMenu_f` (menu.c), what Escape does outside the menu and the
    /// `togglemenu` command: over the game the main menu opens; with the
    /// console down, the console goes up (`Con_ToggleConsole_f`); within the
    /// menu a submenu returns to Main and Main closes (without `M_Main_Key`'s
    /// demo-loop resume).
    pub(crate) fn m_toggle_menu(&mut self) {
        match self.key_dest() {
            KeyDest::Menu => {
                let _ = self.menu.toggle();
            }
            KeyDest::Console => self.toggle_console(),
            KeyDest::Game => self.m_menu_main(),
        }
    }

    /// `cls.demoplayback`: a demo is the active mode.
    pub(crate) fn demoplayback(&self) -> bool {
        self.mode == 1 && self.demo.is_some()
    }

    /// `sv.active`: a local game is the active mode.
    pub(crate) fn sv_active(&self) -> bool {
        self.mode == 0 && self.walk.is_some()
    }

    /// Start playing `walk`, the way `map` / `load` / New Game start a game:
    /// `cls.demonum = -1` ("stop demo loop in case this fails") and
    /// `CL_Disconnect` from any demo (a timedemo prints its line) — then the
    /// walk is the active mode.
    pub(crate) fn start_game(&mut self, walk: Walk) {
        self.cls.demonum = -1;
        crate::cl_demo::cl_stop_playback(self);
        self.cls.timedemo = false;
        self.demo = None;
        self.walk = Some(walk);
        self.mode = 0;
        self.disconnected = false;
    }

    /// The palette of the active mode (the walk's, or the demo's), for the menu
    /// overlay. `None` when no mode has a scene yet (then there is nothing to
    /// overlay the menu onto anyway).
    pub(crate) fn active_palette(&self) -> Option<&[[u8; 3]; 256]> {
        let mode = if self.mode == 1 {
            self.demo.as_ref().map(|d| &d.palette)
        } else {
            self.walk.as_ref().map(|w| &w.palette)
        };
        // Disconnected, nothing has a scene: the pak's palette.
        mode.or(if self.disconnected { self.palette.as_ref() } else { None })
    }

    /// `Con_ToggleConsole_f` (console.c): the console goes down, or up — the
    /// typing cleared — or, with nothing playing (disconnected), the main
    /// menu comes up in its place (there is no game to go back to); and
    /// `con_times` is zeroed — nothing printed so far shows as a notify line
    /// afterwards. The console key's `toggleconsole` binding and Options >
    /// "Go to console" (M_Options_Key) run it.
    pub(crate) fn toggle_console(&mut self) {
        if self.console.open && self.disconnected {
            self.console.open = false;
            self.m_menu_main();
            let _ = self.console.take_unnotified();
        } else {
            self.console.toggle();
        }
        if let Some(w) = self.walk.as_mut() {
            w.notify.clear();
        }
        if let Some(d) = self.demo.as_mut() {
            d.notify.clear();
        }
    }

    /// `Con_Print`'s `con_times` for what the host printed on the console
    /// ([`Console::take_unnotified`](quake_rs::console::Console::take_unnotified)):
    /// the active mode's notify lines get it, stamped on the clock they age
    /// on, so "Saving game to s0.sav..." after a menu save shows over the game
    /// as in the C. Run after every [`ensure_app`] call, i.e. as soon as the
    /// text is printed: a level load that follows (a fresh mode, whose notify
    /// lines start empty) drops it, as SCR_EndLoadingPlaque's Con_ClearNotify
    /// does.
    fn con_notify(&mut self) {
        let text = self.console.take_unnotified();
        if text.is_empty() {
            return;
        }
        if self.mode == 1 {
            if let Some(d) = self.demo.as_mut() {
                let now = d.demo.frames.get(d.idx).map_or(0.0, |f| f.time);
                d.notify.lay(&text, now);
            }
        } else if let Some(w) = self.walk.as_mut() {
            w.notify.lay(&text, w.host_time);
        }
    }
}

thread_local! {
    pub(crate) static APP: RefCell<Option<App>> = const { RefCell::new(None) };
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
        if classname == "info_player_start"
            && let Some(o) = origin
        {
            return Some((o, angle));
        }
    }
    None
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
        textbox: std::array::from_fn(|i| lmp(quake_rs::menu::TEXTBOX_PICS[i])),
        bigbox: lmp("gfx/bigbox.lmp"),
        menuplyr: lmp("gfx/menuplyr.lmp"),
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

/// Build a live walk on `map` (a `maps/*.bsp` pak path) from the embedded
/// pak ([`quake_rs::client::host_cmd::build_walk_map`]), its sound calls
/// carried out: boot uses [`WALK_MAP`]; New Game uses
/// [`render::NEW_GAME_MAP`] (the `start` hub).
pub(crate) fn build_walk_map(map: &str) -> Option<Walk> {
    let pak = pak()?;
    let mut sound = Vec::new();
    let walk = host_cmd::build_walk_map(pak.clone(), map, &mut sound);
    snd_dma::play(&pak, sound);
    walk
}

/// The first attract demo (`demo1`).
#[cfg(test)]
pub(crate) fn build_demo() -> Option<DemoPlay> {
    build_demo_n(0)
}

/// `playdemo` of [`DEMOS`](cl_demo::DEMOS)`[demonum % 3]` from the embedded pak
/// ([`quake_rs::client::cl_demo::build_demo_n`]), its sound calls carried out.
#[cfg(test)]
pub(crate) fn build_demo_n(demonum: usize) -> Option<DemoPlay> {
    let pak = pak()?;
    let mut sound = Vec::new();
    let demo = cl_demo::build_demo_n(pak.clone(), demonum, &mut sound);
    snd_dma::play(&pak, sound);
    demo
}

/// The demo file `name` from the embedded pak — for `playdemo`
/// ([`cl_demo::build_demo`]) or for `timedemo`
/// ([`cl_demo::build_timedemo`]) — its sound calls carried out.
pub(crate) fn build_demo_file(name: &str, timedemo: bool) -> Option<DemoPlay> {
    let pak = pak()?;
    let mut sound = Vec::new();
    let demo = if timedemo {
        cl_demo::build_timedemo(pak.clone(), name, &mut sound)
    } else {
        cl_demo::build_demo(pak.clone(), name, &mut sound)
    };
    snd_dma::play(&pak, sound);
    demo
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
                host_framecount: 0,
                cls: Cls::default(),
                disconnected: false,
                palette: None,
                key_repeats: [0; 256],
                shift_down: false,
                m_save_demonum: 0,
                render_threads: render::Threads::Auto,
                hw_threads: 1,
                video: render::VideoCvars::CLASSIC,
            });
        }
        if let Some(a) = c.borrow_mut().as_mut() {
            f(a);
            a.con_notify();
        }
    });
}

// ---------------------------------------------------------------------------
// Exports
// ---------------------------------------------------------------------------

/// Start interactive walk mode (e1m1). Returns 1 on success.
pub(crate) fn boot() -> i32 {
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
            a.start_game(w);
            // Quake boots INTO the menu over the e1m1 frame. A program start's
            // NAVIGATION (closed, main screen, every cursor 0: menu.c's statics)
            // and the menu opened over the walk — but KEEP the player's options,
            // key rebinds, and slot comments: in the C a map start never touches
            // cvars/keybindings (they're host state), so re-booting must not
            // wipe them.
            a.menu.reset_boot();
            a.m_menu_main();
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
/// (quake.rc's startdemos cycle, see [`DEMOS`](cl_demo::DEMOS)). Returns 1 on success.
pub(crate) fn boot_demo() -> i32 {
    // Clean slate: drop any sounds still queued from a previous mode
    // (pending stop requests included).
    SND_QUEUE.with(|q| q.borrow_mut().clear());
    STOP_SND_QUEUE.with(|q| q.borrow_mut().clear());
    let mut ok = false;
    ensure_app(|a| {
        a.ensure_menu_assets();
        ok = start_attract_loop(a);
        if ok {
            // The demo button plays the demo with the menu CLOSED (clean
            // playback). `boot_attract` is the variant that opens the menu over it.
            // Navigation-only reset: options/bindings/slot comments survive (the
            // C never resets cvars or keybindings on a mode change). The menu
            // cursors too (menu.c's statics; playdemo keeps them). PRESERVE the
            // chosen resolution too (keep the live framebuffer) and point the
            // menu's current video mode at it so it's correct when the player
            // next opens Video Options.
            a.menu.reset_nav();
            a.menu.sync_resolution(a.render_w as i32, a.render_h as i32);
        }
    });
    ok as i32
}

/// quake.rc's `startdemos demo1 demo2 demo3` as the page's boots run it:
/// whatever was running is disconnected (so the loop always starts —
/// `Host_Startdemos_f` itself only starts it with nothing running), the loop
/// is set and `CL_NextDemo` plays demo1. True when it plays.
fn start_attract_loop(a: &mut App) -> bool {
    crate::cl_demo::cl_disconnect(a);
    a.cls.demonum = 0;
    crate::host_cmd::host_startdemos(a, &QUAKE_RC_DEMOS);
    a.demoplayback()
}

/// Boot into the ATTRACT loop, as Quake starts: quake.rc's `startdemos demo1
/// demo2 demo3` plays with no menu (`key_dest` starts at `key_game`), and any
/// key brings the main menu up (`Key_Event` during demo playback). The page
/// calls this on load instead of [`boot`]. Returns 1 when the demo built, or 0
/// when it could not — in which case we fall back to [`boot`] so the user still
/// lands on a menu over *something* (e1m1) rather than a blank screen.
pub(crate) fn boot_attract() -> i32 {
    // Clean slate: drop any sounds still queued from a previous mode
    // (pending stop requests included).
    SND_QUEUE.with(|q| q.borrow_mut().clear());
    STOP_SND_QUEUE.with(|q| q.borrow_mut().clear());
    let mut built = false;
    ensure_app(|a| {
        a.ensure_menu_assets();
        built = start_attract_loop(a);
        if built {
            // The page's load is the program start: the menu closed, every
            // cursor 0 (navigation only: options/bindings survive a re-entry
            // to the attract loop). PRESERVE the chosen resolution (keep the
            // live framebuffer) and sync the menu's current video mode to it.
            // On the very first load the framebuffer is at DEFAULT; the page
            // then restores any saved resolution over it.
            a.menu.reset_boot();
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
pub(crate) fn in_walk_mode() -> i32 {
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

    /// menu.c's cursors are statics: `map`, New Game, a load and a demo keep
    /// them (Escape afterwards reopens Main on "Options" and Options on its
    /// row); only a program start — the page's load and walk button, `boot` —
    /// has them at 0.
    #[test]
    fn only_a_boot_resets_the_menu_cursors() {
        let cursor = || APP.with(|c| c.borrow().as_ref().unwrap().menu.cursor());
        assert_eq!(boot(), 1);
        menu_down();
        menu_down();
        menu_select(); // Options
        for _ in 0..3 {
            menu_down();
        }
        menu_cancel(); // Main, on "Options"
        menu_cancel(); // closed
        crate::host_cmd::execute_console_command("map e1m2");
        menu_cancel(); // Escape: M_Menu_Main_f
        assert_eq!(cursor(), 2, "m_main_cursor survives `map`");
        menu_select();
        assert_eq!(cursor(), 3, "options_cursor survives `map`");
        menu_cancel();
        menu_cancel();
        assert_eq!(boot_demo(), 1);
        menu_cancel();
        assert_eq!(cursor(), 2, "and the demo");
        assert_eq!(boot(), 1);
        assert_eq!(cursor(), 0, "a program start");
        menu_down();
        menu_down();
        menu_select();
        assert_eq!(cursor(), 0);
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
    fn attract_loop_cycles_demo1_demo2_demo3() {
        // quake.rc `startdemos demo1 demo2 demo3`: each demo's svc_disconnect
        // runs CL_NextDemo, so the attract loop plays the three in turn and
        // wraps — not demo1 forever.
        assert_eq!(boot_attract(), 1);
        // (the demo playing: the one before cls.demonum, the loop's next)
        let demo = || {
            APP.with(|c| {
                let b = c.borrow();
                let a = b.as_ref().unwrap();
                let d = a.demo.as_ref().unwrap();
                (a.cls.demonum - 1, d.demo.map_name().unwrap_or("").to_string(), d.idx)
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
        assert_eq!(menu_visible(), 0, "no menu over the loop");
    }

    /// Final review (UI): Quake starts with `key_dest = key_game` — the
    /// attract demo plays with no menu — and during demo playback a console
    /// key (Key_Event: `cls.demoplayback && consolekeys[key] && key_dest ==
    /// key_game`) brings the main menu up; Escape too (M_ToggleMenu_f). The
    /// mouse buttons and the F-keys are no console keys: they don't.
    #[test]
    fn boot_attract_plays_the_demo_and_a_key_brings_up_the_menu() {
        use quake_rs::keys::{K_ENTER, K_F1, K_MOUSE1};
        assert_eq!(boot_attract(), 1, "attract built the demo from the embedded pak");
        let (mode, has_walk, has_demo, vis) = app_state();
        assert_eq!(mode, 1, "attract boots into demo mode");
        assert!(has_demo, "the demo was built");
        assert!(!has_walk, "no walk is built for the attract demo");
        assert!(!vis, "no menu: key_dest starts at key_game");
        let idx = || APP.with(|c| c.borrow().as_ref().unwrap().demo.as_ref().unwrap().idx);
        let idx_before = idx();
        for _ in 0..20 {
            step(0.05);
        }
        assert_ne!(idx_before, idx(), "the demo plays");
        for k in [K_MOUSE1, K_F1 + 4] {
            crate::input::press(k);
            assert_eq!(menu_visible(), 0, "key {k} is no console key");
        }
        for k in [b'x', b' ', K_ENTER, b'1'] {
            crate::input::press(k);
            assert_eq!(menu_visible(), 1, "key {k} brings up the menu");
            assert_eq!(menu_screen(), render::MenuScreen::Main);
            menu_cancel();
            assert_eq!(menu_visible(), 0);
        }
        menu_cancel();
        assert_eq!(menu_visible(), 1, "Escape too");
        // The demo goes on playing behind the menu.
        let idx_before = idx();
        for _ in 0..20 {
            step(0.05);
        }
        assert_ne!(idx_before, idx(), "the attract demo keeps playing behind the menu");
        assert_eq!(menu_visible(), 1, "the menu remains open over the demo");
    }

    /// M_Menu_Main_f: `m_save_demonum = cls.demonum; cls.demonum = -1` — the
    /// menu stops the attract loop: the demo playing ends in CL_Disconnect
    /// (the console covers the screen, the menu over it, drawn over the
    /// console background as M_Draw does with `scr_con_current`), and
    /// M_Main_Key's Escape puts the loop back and plays its next demo. Closed
    /// before the demo ends, the loop just goes on.
    #[test]
    fn the_menu_stops_the_attract_loop_until_escape() {
        assert_eq!(boot_attract(), 1);
        let demonum = || APP.with(|c| c.borrow().as_ref().unwrap().cls.demonum);
        let to_end = || {
            APP.with(|c| {
                let mut b = c.borrow_mut();
                let d = b.as_mut().unwrap().demo.as_mut().unwrap();
                d.idx = d.demo.frames.len() - 1;
            })
        };
        assert_eq!(demonum(), 1, "demo1 plays, demos[1] next");
        crate::input::press(b'm');
        assert_eq!(demonum(), -1, "the menu switched the loop off");
        // Closed again before the demo ends: the loop is back, untouched.
        menu_cancel();
        assert_eq!((menu_visible(), demonum()), (0, 1));
        // Up again, and the demo ends under it: disconnected, not demo2.
        crate::input::press(b'm');
        to_end();
        step(0.05);
        let (mode, has_walk, has_demo, vis) = app_state();
        assert_eq!((mode, has_walk, has_demo, vis), (1, false, false, true), "disconnected, menu up");
        let (disconnected, console) = APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            (a.disconnected, a.console.current())
        });
        assert!(disconnected);
        let h = APP.with(|c| c.borrow().as_ref().unwrap().render_h) as f32;
        assert_eq!(console, h, "con_forcedup behind the menu");
        // M_Draw with scr_con_current: the console background under the menu
        // at full height, not the faded console text.
        let fb = APP.with(|c| c.borrow().as_ref().unwrap().fb.clone());
        let conback_only = APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            let mut img = render::Image::new(a.render_w, a.render_h, [0, 0, 0]);
            let pal = a.palette.as_ref().unwrap();
            quake_rs::console::draw_console_background_full(&mut img, a.conback.as_ref(), a.conchars.as_ref(), pal);
            img.rgb
        });
        let w = APP.with(|c| c.borrow().as_ref().unwrap().render_w);
        let bottom = (h as usize - 1) * w;
        assert!(
            (0..w).all(|x| fb[(bottom + x) * 4..(bottom + x) * 4 + 3] == conback_only[bottom + x]),
            "the bottom row is the plain console background"
        );
        // Escape from Main: the loop back, its next demo plays.
        menu_cancel();
        assert_eq!(menu_visible(), 0);
        let (map, n) = APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            (a.demo.as_ref().and_then(|d| d.demo.map_name().map(str::to_string)), a.cls.demonum)
        });
        assert!(map.is_some(), "a demo plays again");
        assert_eq!(n, 2, "demos[1] (demo2) played");
    }

    #[test]
    fn boot_attract_new_game_switches_to_walk_with_menu_closed() {
        // From the attract loop, a key brings up the menu and Single Player >
        // New Game starts a fresh walk on the start hub and closes the menu.
        // Drive the same key path the page uses.
        assert_eq!(boot_attract(), 1);
        assert_eq!(app_state(), (1, false, true, false), "attract: demo mode, no menu");
        crate::input::press(b'a');
        assert_eq!(app_state(), (1, false, true, true), "a key: the menu");

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
        menu_cancel(); // the menu over the demo
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
        menu_cancel(); // the menu over the demo
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
