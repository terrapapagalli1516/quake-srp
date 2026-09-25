//! The render profiler: per-phase timers and counters for one frame.
//!
//! The port's own (id has `r_speeds`); off unless [`render_stats_begin`] turns it
//! on, so the game pays nothing.

thread_local! {
    /// Granular render profiler (opt-in; see [`RenderStats`]). Off by default so the
    /// shared render path pays nothing in the live game / wasm.
    static RENDER_STATS: std::cell::RefCell<RenderStats> =
        const { std::cell::RefCell::new(RenderStats::ZERO) };
    static STATS_ON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Granular per-phase render profiler — phase wall-times (ns) plus face/triangle/
/// pixel/cache counts for one [`render_scene_ext_sprited`] call. Populated only
/// while profiling is enabled via [`render_stats_begin`]; every counter site is
/// gated on the `STATS_ON` flag, so a normal (game/wasm) render touches none of it.
/// Use this to see WHERE a frame's time goes (which phase, overdraw, cache hit rate)
/// when tuning performance.
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderStats {
    /// Per-phase wall time in nanoseconds.
    pub world_ns: u64,
    pub submodel_ns: u64,
    pub external_ns: u64,
    pub alias_ns: u64,
    pub particle_ns: u64,
    pub sprite_ns: u64,
    pub viewmodel_ns: u64,
    /// World-model faces in the model-0 range.
    pub faces_total: u64,
    /// Faces skipped by the PVS visibility mask.
    pub faces_pvs_culled: u64,
    /// Faces skipped by the view-frustum AABB cull.
    pub faces_frustum_culled: u64,
    /// Faces that reached the rasteriser (world pass).
    pub faces_drawn: u64,
    /// Triangles submitted by the world pass.
    pub world_tris: u64,
    /// Pixels actually written by the world pass (overdraw proxy: a pixel covered by
    /// N drawn surfaces counts N times).
    pub world_pixels: u64,
    /// Lit-surface-cache hits / misses (world pass).
    pub surf_hits: u64,
    pub surf_misses: u64,
    /// Submodel pass: faces drawn, triangles, and pixels written. The submodel
    /// pass currently has NO surface cache and NO front-to-back ordering, so these
    /// reveal how much of the (often surprisingly large) submodel time is overdraw
    /// vs per-pixel lightmap+colormap cost — the next optimization target.
    pub sub_faces_visited: u64,
    pub sub_faces_drawn: u64,
    pub sub_surf_hits: u64,
    pub sub_surf_misses: u64,
    pub sub_tris: u64,
    /// Submodel lightmap rebuilds (every submodel face rebuilds via
    /// `face_lightmap_dyn` each frame — no cache).
    pub sub_lm_builds: u64,
    /// World-pass sub-phase timers (ns), for finding the FIXED per-face cost that
    /// dominates the frame independent of resolution. Only populated while
    /// profiling. `world_pvs_ns` is the once-per-frame PVS+frustum build; the rest
    /// accumulate across the per-face loop. `world_setup_ns` is the loop-body
    /// remainder (geom fetch + culls + projection + the per-pixel raster, since
    /// raster is not separately metered) and so also absorbs the `Instant` overhead
    /// of the nested light/surf timers — read it as "everything that isn't lightmap
    /// or surf-block lookup", not a precise figure.
    pub world_pvs_ns: u64,
    pub world_sort_ns: u64,
    pub world_setup_ns: u64,
    pub world_light_ns: u64,
    pub world_surf_ns: u64,
    /// Lit-surface-cache accounting inside `face_surf_block` (distinct from
    /// `surf_hits`, which only counts "returned a block"):
    ///  * `surf_cache_hits` — served from the world cache (the warm-frame norm).
    ///  * `surf_baked` — CACHED-path misses that re-baked a block. On a warm frame
    ///    this should be ~0; a high value means the world cache is mismatching/being
    ///    evicted (the dominant fixed per-face cost — a bug).
    ///  * `surf_bypass_baked` — external brush models, which intentionally bypass
    ///    the cache and bake fresh each frame (cheap; one per visible item-box face).
    pub surf_cache_hits: u64,
    pub surf_baked: u64,
    pub surf_bypass_baked: u64,
}

impl RenderStats {
    const ZERO: RenderStats = RenderStats {
        world_ns: 0, submodel_ns: 0, external_ns: 0, alias_ns: 0, particle_ns: 0,
        sprite_ns: 0, viewmodel_ns: 0, faces_total: 0, faces_pvs_culled: 0,
        faces_frustum_culled: 0, faces_drawn: 0, world_tris: 0, world_pixels: 0,
        surf_hits: 0, surf_misses: 0,
        sub_faces_visited: 0, sub_faces_drawn: 0, sub_surf_hits: 0, sub_surf_misses: 0,
        sub_tris: 0, sub_lm_builds: 0,
        world_pvs_ns: 0, world_sort_ns: 0, world_setup_ns: 0, world_light_ns: 0,
        world_surf_ns: 0,
        surf_cache_hits: 0, surf_baked: 0, surf_bypass_baked: 0,
    };
}

/// Enable the render profiler and clear its counters. The NEXT
/// [`render_scene_ext_sprited`] accumulates into [`RenderStats`]; read + disable
/// with [`render_stats_end`]. Intended for the `quaketool` benchmark, not the game.
pub fn render_stats_begin() {
    RENDER_STATS.with(|s| *s.borrow_mut() = RenderStats::ZERO);
    STATS_ON.with(|c| c.set(true));
}

/// Read the accumulated [`RenderStats`] and disable the profiler.
pub fn render_stats_end() -> RenderStats {
    STATS_ON.with(|c| c.set(false));
    RENDER_STATS.with(|s| *s.borrow())
}

/// Whether the render profiler is currently accumulating (cheap `Cell` read).
#[inline]
pub(super) fn stats_on() -> bool {
    STATS_ON.with(|c| c.get())
}

/// Apply `f` to the live [`RenderStats`] iff profiling is on (no-op otherwise).
#[inline]
pub(super) fn stat(f: impl FnOnce(&mut RenderStats)) {
    if stats_on() {
        RENDER_STATS.with(|s| f(&mut s.borrow_mut()));
    }
}

thread_local! {
    /// Clock override for the [`RenderStats`] phase timers: a monotonic
    /// milliseconds source (e.g. the browser's `performance.now()`), for targets
    /// where `std::time::Instant` is unavailable (`wasm32-unknown-unknown` panics
    /// on it). `None` (the default) uses `Instant`.
    static STATS_CLOCK: std::cell::Cell<Option<fn() -> f64>> = const { std::cell::Cell::new(None) };
}

/// Install (or clear) the [`RenderStats`] timer clock — a monotonic milliseconds
/// source. The wasm shell's opt-in benchmark build passes `performance.now()`
/// here so [`render_stats_begin`] works in the browser; native callers never
/// need it. Only read while profiling is on, so the game pays nothing.
pub fn set_render_stats_clock(clock: Option<fn() -> f64>) {
    STATS_CLOCK.with(|c| c.set(clock));
}

/// A profiler timestamp: `std::time::Instant`, or a reading of the clock
/// installed by [`set_render_stats_clock`]. Only constructed while profiling.
#[derive(Clone, Copy)]
pub(super) enum StatInstant {
    Std(std::time::Instant),
    Ms(fn() -> f64, f64),
}

impl StatInstant {
    pub(super) fn now() -> StatInstant {
        match STATS_CLOCK.with(|c| c.get()) {
            Some(clock) => StatInstant::Ms(clock, clock()),
            None => StatInstant::Std(std::time::Instant::now()),
        }
    }

    pub(super) fn elapsed(&self) -> std::time::Duration {
        match *self {
            StatInstant::Std(t) => t.elapsed(),
            StatInstant::Ms(clock, t0) => {
                std::time::Duration::from_nanos(((clock() - t0).max(0.0) * 1.0e6) as u64)
            }
        }
    }
}
