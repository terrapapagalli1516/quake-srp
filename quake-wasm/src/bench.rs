//! Opt-in frame-phase timers for the browser benchmark (`web/bench.py`).
//!
//! Built only with `--features bench`. The default build (the deployed page)
//! compiles every hook below to an empty inline function: no clock import, no
//! extra exports, no state. With the feature on, the module imports ONE
//! function, `quake_bench.now_ms` (the page's `performance.now()`, supplied by
//! the harness — the stock page instantiates with no imports, so the feature
//! build is for the harness only), and [`crate::host::step`] laps a timer at each
//! phase boundary of the frame:
//!
//! | phase    | what it covers (C analogue)                                        |
//! |----------|--------------------------------------------------------------------|
//! | input    | `step` prologue: menu gate + bindings (`CL_BaseMove`)              |
//! | sim      | walk: `SV_Physics` tick + client-side drains/effects/entity list;  |
//! |          | demo: `CL_ReadFromServer` frame advance + effects + entity list    |
//! | render3d | `R_RenderView` (`render_scene_ext_sprited`), split further by the  |
//! |          | engine's `RenderStats` into world/submodel/external/alias/… and    |
//! |          | the world pass into pvs/sort/setup(+raster)/light/surf             |
//! | post3d   | `D_WarpScreen`, the view composed into the screen, the cshifts     |
//! | hud2d    | `Sbar_Draw` / intermission overlays + centerprint/notify           |
//! | menu     | `M_Draw`                                                           |
//! | console  | `Con_DrawConsole`                                                  |
//! | blend    | `V_UpdatePalette`: the cshift + gamma ramps (256 entries each)     |
//! | pack     | RGB -> RGBA through the ramps into the presented framebuffer       |
//!
//! Timing never changes what is drawn: the laps only read the clock, and the
//! engine's `RenderStats` counters are the same ones `quaketool`'s
//! `QUAKE_BENCH` prints (gated on a flag the game never sets).

/// A frame phase, in execution order; see the module table.
#[derive(Clone, Copy)]
pub(crate) enum Phase {
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

/// Start a frame's timers (the top of `step`).
#[inline(always)]
pub(crate) fn frame_begin() {
    #[cfg(feature = "bench")]
    imp::frame_begin();
}

/// Charge the time since the previous lap to `phase`.
#[inline(always)]
pub(crate) fn lap(_phase: Phase) {
    #[cfg(feature = "bench")]
    imp::lap(_phase);
}

/// Close the frame (the end of `step`): latch its phase times and render stats.
#[inline(always)]
pub(crate) fn frame_end() {
    #[cfg(feature = "bench")]
    imp::frame_end();
}

#[cfg(feature = "bench")]
mod imp {
    use super::Phase;
    use quake_rs::render::{self, RenderStats};
    use std::cell::{Cell, RefCell};

    const N_PHASES: usize = 9;

    /// Value names, in `bench_value` index order: the shell phases (ms), the
    /// engine's render phases (ms), then the render counters.
    pub(super) const NAMES: &str = "input,sim,render3d,post3d,hud2d,menu,console,blend,pack,\
world,submodel,external,alias,particle,sprite,viewmodel,\
world_pvs,world_sort,world_setup,world_light,world_surf,\
faces_pvs_culled,faces_frustum_culled,faces_drawn,world_tris,world_px,surf_hits,surf_misses,\
surf_rebakes,surf_bypass_bakes,sub_faces_drawn,sub_lm_builds";

    #[cfg(target_arch = "wasm32")]
    #[link(wasm_import_module = "quake_bench")]
    unsafe extern "C" {
        /// The page's `performance.now()` (milliseconds), supplied by `web/bench.py`.
        #[link_name = "now_ms"]
        safe fn js_now_ms() -> f64;
    }

    /// The benchmark clock, in milliseconds: the page's `performance.now()`.
    #[cfg(target_arch = "wasm32")]
    fn now_ms() -> f64 {
        js_now_ms()
    }

    /// Native builds (the `native_bench` test) time with `Instant`.
    #[cfg(not(target_arch = "wasm32"))]
    fn now_ms() -> f64 {
        thread_local! {
            static EPOCH: std::time::Instant = std::time::Instant::now();
        }
        EPOCH.with(|e| e.elapsed().as_secs_f64() * 1000.0)
    }

    thread_local! {
        static ON: Cell<bool> = const { Cell::new(false) };
        static LAST: Cell<f64> = const { Cell::new(0.0) };
        static ACC: RefCell<[f64; N_PHASES]> = const { RefCell::new([0.0; N_PHASES]) };
        static DONE: RefCell<([f64; N_PHASES], RenderStats)> =
            RefCell::new(([0.0; N_PHASES], RenderStats::default()));
    }

    pub(super) fn frame_begin() {
        if !ON.with(|c| c.get()) {
            return;
        }
        ACC.with(|a| *a.borrow_mut() = [0.0; N_PHASES]);
        render::render_stats_begin();
        LAST.with(|l| l.set(now_ms()));
    }

    pub(super) fn lap(phase: Phase) {
        if !ON.with(|c| c.get()) {
            return;
        }
        let t = now_ms();
        let dt = t - LAST.with(|l| l.replace(t));
        ACC.with(|a| a.borrow_mut()[phase as usize] += dt);
    }

    pub(super) fn frame_end() {
        if !ON.with(|c| c.get()) {
            return;
        }
        let stats = render::render_stats_end();
        let acc = ACC.with(|a| *a.borrow());
        DONE.with(|d| *d.borrow_mut() = (acc, stats));
    }

    /// Enable (1) or disable (0) the timers. Enabling installs the clock the
    /// engine's `RenderStats` phase timers read.
    #[no_mangle]
    pub extern "C" fn bench_enable(on: i32) {
        render::set_render_stats_clock(if on != 0 { Some(now_ms) } else { None });
        ON.with(|c| c.set(on != 0));
    }

    /// Byte length of the comma-separated value-name list at [`bench_names_ptr`].
    #[no_mangle]
    pub extern "C" fn bench_names_len() -> i32 {
        NAMES.len() as i32
    }

    /// The comma-separated value names (ASCII), in [`bench_value`] index order.
    #[no_mangle]
    pub extern "C" fn bench_names_ptr() -> *const u8 {
        NAMES.as_ptr()
    }

    /// Value `i` of the last completed frame (ms for phases, a count for the
    /// counters); `NaN` past the end.
    #[no_mangle]
    pub extern "C" fn bench_value(i: i32) -> f64 {
        DONE.with(|d| {
            let (ph, s) = &*d.borrow();
            let ms = |ns: u64| ns as f64 / 1.0e6;
            let i = i.max(0) as usize;
            if i < N_PHASES {
                return ph[i];
            }
            let rest = [
                ms(s.world_ns),
                ms(s.submodel_ns),
                ms(s.external_ns),
                ms(s.alias_ns),
                ms(s.particle_ns),
                ms(s.sprite_ns),
                ms(s.viewmodel_ns),
                ms(s.world_pvs_ns),
                ms(s.world_sort_ns),
                ms(s.world_setup_ns),
                ms(s.world_light_ns),
                ms(s.world_surf_ns),
                s.faces_pvs_culled as f64,
                s.faces_frustum_culled as f64,
                s.faces_drawn as f64,
                s.world_tris as f64,
                s.world_pixels as f64,
                s.surf_hits as f64,
                s.surf_misses as f64,
                s.surf_baked as f64,
                s.surf_bypass_baked as f64,
                s.sub_faces_drawn as f64,
                s.sub_lm_builds as f64,
            ];
            rest.get(i - N_PHASES).copied().unwrap_or(f64::NAN)
        })
    }
}

/// The native half of `web/bench.py`: the SAME workloads, driven through the
/// SAME exports the page calls, on the host CPU — so every browser phase has a
/// native twin and the wasm/native ratio is per phase, not a guess.
///
/// `cargo test --release --features bench --lib -- --ignored --nocapture native_bench`
/// Knobs (env): `QUAKE_BENCH_WORKLOADS` (comma list, default `demo1,walk_e1m1`),
/// (`walk_<map>` / `fire_<map>` / `quad_<map>` = the scripted live walk, `fire_` with
/// +attack held, `quad_` after `impulse 255` so the Quad's cshift is on),
/// `QUAKE_BENCH_RES` (comma list of WxH, default `320x200,640x400,1280x800`),
/// `QUAKE_BENCH_FRAMES` (default 600), `QUAKE_BENCH_WARMUP` (default 60).
/// Prints one `BENCHJSON {...}` line per (workload, resolution) with per-frame
/// arrays for every named value, for bench.py to merge into its table.
#[cfg(all(test, feature = "bench"))]
mod native {
    use super::imp::{bench_enable, bench_value, NAMES};
    use crate::app::{boot, boot_attract, boot_demo, in_walk_mode};
    use crate::console::{console_char, console_enter, console_toggle, console_visible};
    use crate::host::step;
    use crate::input::{look, set_attack, set_move};
    use crate::menu::{menu_cancel, menu_visible};
    use crate::vid::set_resolution;

    /// Quake's frame cadence (`host_maxfps` 72): every workload steps at it.
    const DT: f32 = 1.0 / 72.0;

    /// The scripted input for live-walk frame `f` — the same script
    /// `web/bench.py` runs (`walkInput` there): a 720-frame (10 s) cycle that
    /// looks everywhere and comes back — a slow 360 degree sweep in place, run
    /// forward 1 s, about-face, run back, about-face, idle. Returns (forward
    /// fraction, yaw degrees to turn RIGHT this frame).
    fn walk_input(f: u32) -> (f32, f32) {
        match f % 720 {
            0..=359 => (0.0, 1.0),
            360..=431 => (1.0, 0.0),
            432..=503 => (0.0, 2.5),
            504..=575 => (1.0, 0.0),
            576..=647 => (0.0, 2.5),
            _ => (0.0, 0.0),
        }
    }

    /// The live-walk workloads (`isWalk` in `web/bench.py`).
    fn is_walk(wl: &str) -> bool {
        wl.starts_with("walk_") || wl.starts_with("fire_") || wl.starts_with("quad_")
    }

    /// Boot `workload` exactly as `web/bench.py`'s `startWorkload` does.
    fn start(workload: &str) -> bool {
        match workload {
            "attract" => boot_attract() == 1,
            "demo1" => boot_demo() == 1,
            w if is_walk(w) => {
                let map = &w["walk_".len()..];
                if boot() != 1 {
                    return false;
                }
                if map != "e1m1" {
                    console_toggle();
                    for ch in format!("map {map}").chars() {
                        console_char(ch as u32);
                    }
                    console_enter();
                    if console_visible() != 0 {
                        console_toggle();
                    }
                }
                // boot() lands in the main menu; close it (Esc) so the view is
                // the game and input is not gated.
                if menu_visible() != 0 {
                    menu_cancel();
                }
                if w.starts_with("quad_") {
                    // id's QuadCheat (weapons.qc, impulse 255): IT_QUAD for 30 s.
                    console_toggle();
                    for ch in "impulse 255".chars() {
                        console_char(ch as u32);
                    }
                    console_enter();
                    if console_visible() != 0 {
                        console_toggle();
                    }
                }
                in_walk_mode() == 1 && menu_visible() == 0
            }
            _ => false,
        }
    }

    #[test]
    #[ignore = "benchmark: run explicitly (see module docs)"]
    fn native_bench() {
        let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
        let workloads = env("QUAKE_BENCH_WORKLOADS", "demo1,walk_e1m1");
        let resolutions = env("QUAKE_BENCH_RES", "320x200,640x400,1280x800");
        let frames: u32 = env("QUAKE_BENCH_FRAMES", "600").parse().unwrap_or(600);
        let warmup: u32 = env("QUAKE_BENCH_WARMUP", "60").parse().unwrap_or(60);
        let names: Vec<&str> = NAMES.split(',').collect();
        for wl in workloads.split(',') {
            for res in resolutions.split(',') {
                let mut it = res.split('x');
                let (w, h): (i32, i32) = (
                    it.next().and_then(|v| v.parse().ok()).unwrap_or(640),
                    it.next().and_then(|v| v.parse().ok()).unwrap_or(400),
                );
                assert!(start(wl), "workload {wl} failed to boot");
                set_resolution(w, h);
                bench_enable(1);
                let mut cols: Vec<Vec<f64>> = vec![Vec::new(); names.len() + 1];
                for f in 0..warmup + frames {
                    if is_walk(wl) {
                        let (fwd, turn) = walk_input(f);
                        set_move(fwd, 0.0);
                        look(-turn, 0.0);
                        set_attack(wl.starts_with("fire_") as i32);
                    }
                    let t0 = std::time::Instant::now();
                    step(DT);
                    let total = t0.elapsed().as_secs_f64() * 1000.0;
                    if f >= warmup {
                        cols[0].push(total);
                        for (i, c) in cols.iter_mut().skip(1).enumerate() {
                            c.push(bench_value(i as i32));
                        }
                    }
                }
                bench_enable(0);
                let mut json = format!(
                    "{{\"side\":\"native\",\"workload\":\"{wl}\",\"w\":{w},\"h\":{h},\"values\":{{\"step\":{:?}",
                    cols[0]
                );
                for (i, n) in names.iter().enumerate() {
                    json.push_str(&format!(",\"{n}\":{:?}", cols[i + 1]));
                }
                json.push_str("}}");
                println!("BENCHJSON {json}");
            }
        }
    }
}
