//! The render profiler: per-phase timers and counters for one frame.
//!
//! The port's own (id has `r_speeds`); off unless
//! [`Renderer::stats_begin`](super::Renderer::stats_begin) turns it on, so the
//! game pays nothing. It is the [`Renderer`](super::Renderer)'s, like the rest
//! of its state: every pass that counts is handed the [`Profiler`], so a pass
//! running on another thread counts into its own and nothing is lost.

/// Granular per-phase render profiler — phase wall-times (ns) plus face/triangle/
/// pixel/cache counts, summed over the frames [`Renderer::render`](super::Renderer::render)
/// draws while profiling is on ([`Renderer::stats_begin`](super::Renderer::stats_begin)); every counter
/// site is gated on it, so a normal (game/wasm) render touches none of it.
/// Use this to see WHERE a frame's time goes (which phase, overdraw, cache hit rate)
/// when tuning performance.
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderStats {
    /// Per-phase wall time in nanoseconds. `world_ns` is the edge renderer's
    /// whole pass but the brush entities' edge setup, which is `submodel_ns`
    /// (their spans are drawn with the world's); `external_ns` stays 0.
    pub world_ns: u64,
    pub submodel_ns: u64,
    pub external_ns: u64,
    pub alias_ns: u64,
    pub particle_ns: u64,
    pub sprite_ns: u64,
    pub viewmodel_ns: u64,
    /// The polygon walker's face counts (PVS and frustum culls, triangles),
    /// kept for the harnesses' columns: the edge renderer leaves them 0.
    pub faces_total: u64,
    pub faces_pvs_culled: u64,
    pub faces_frustum_culled: u64,
    /// Surfaces the world pass drew (those that own a span; not the background).
    pub faces_drawn: u64,
    pub world_tris: u64,
    /// Pixels the world pass drew: every pixel of the view once, less the
    /// background's.
    pub world_pixels: u64,
    /// Surfaces drawn from a surface-cache block / per pixel.
    pub surf_hits: u64,
    pub surf_misses: u64,
    /// The polygon walker's submodel pass counts (0 with the edge renderer,
    /// whose brush entities are surfaces like the world's), but for
    /// `sub_lm_builds`, the brush-entity lightmaps built per frame.
    pub sub_faces_visited: u64,
    pub sub_faces_drawn: u64,
    pub sub_surf_hits: u64,
    pub sub_surf_misses: u64,
    pub sub_tris: u64,
    pub sub_lm_builds: u64,
    /// World-pass sub-phase timers (ns), only while profiling: `world_sort_ns`
    /// the world walk to edges (`R_RenderWorld`), `world_setup_ns` the scan
    /// (`R_ScanEdges`), `world_surf_ns` `D_DrawSurfaces` (surface cache, spans,
    /// z spans); `world_pvs_ns` and `world_light_ns` are the polygon walker's
    /// and stay 0.
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
    /// Texels those bakes (cached and bypass) filled — at their mip level, so a
    /// distant surface costs a quarter, a sixteenth or a sixty-fourth of its
    /// mip-0 area (`R_DrawSurface`'s work).
    pub surf_texels_baked: u64,
    /// Surfaces asked of the surface cache whose lightmap carries a style
    /// other than 0 — an animated light (`R_AnimateLight`'s patterns) or a
    /// switched one: the blocks a light-style change rebakes.
    pub surf_styled: u64,
    /// Surfaces asked of the surface cache that a steady torch flickers on
    /// (the 2026 `r_torchflicker`): the blocks a torch's change rebakes.
    pub surf_torchlit: u64,
    /// Alias models handed to the renderer (`cl_visedicts` entries that reach
    /// `R_DrawEntitiesOnList`), those `R_AliasCheckBBox` accepted, and the
    /// accepted models' triangles.
    pub alias_models: u64,
    pub alias_accepted: u64,
    pub alias_tris: u64,
    /// The edge renderer (`edge.rs`): edges, surfaces and spans
    /// made per frame (summed), and the most edges / surfaces one frame made —
    /// against id's pools, `r_maxedges` 2400 and `r_maxsurfs` 800 (the
    /// background and the dummy surface not counted), which the port grows.
    pub edges_emitted: u64,
    pub surfs_emitted: u64,
    pub spans_emitted: u64,
    pub edges_peak: u64,
    pub surfs_peak: u64,
    /// The wall time of the frame's banded passes (the world's spans and the
    /// entities, on every thread): with one thread their sum, with several
    /// less — the timers above add each thread's time.
    pub bands_ns: u64,
    /// The threads that drew the bands, summed over the frames.
    pub band_threads: u64,
}

impl RenderStats {
    pub(super) const ZERO: RenderStats = RenderStats {
        world_ns: 0, submodel_ns: 0, external_ns: 0, alias_ns: 0, particle_ns: 0,
        sprite_ns: 0, viewmodel_ns: 0, faces_total: 0, faces_pvs_culled: 0,
        faces_frustum_culled: 0, faces_drawn: 0, world_tris: 0, world_pixels: 0,
        surf_hits: 0, surf_misses: 0,
        sub_faces_visited: 0, sub_faces_drawn: 0, sub_surf_hits: 0, sub_surf_misses: 0,
        sub_tris: 0, sub_lm_builds: 0,
        world_pvs_ns: 0, world_sort_ns: 0, world_setup_ns: 0, world_light_ns: 0,
        world_surf_ns: 0,
        surf_cache_hits: 0, surf_baked: 0, surf_bypass_baked: 0, surf_texels_baked: 0, surf_styled: 0,
        surf_torchlit: 0,
        alias_models: 0, alias_accepted: 0, alias_tris: 0,
        edges_emitted: 0, surfs_emitted: 0, spans_emitted: 0, edges_peak: 0, surfs_peak: 0,
        bands_ns: 0, band_threads: 0,
    };

    /// Add `o`'s counts and times to these (the peaks as the larger).
    fn merge(&mut self, o: &RenderStats) {
        let RenderStats {
            world_ns, submodel_ns, external_ns, alias_ns, particle_ns, sprite_ns, viewmodel_ns,
            faces_total, faces_pvs_culled, faces_frustum_culled, faces_drawn, world_tris, world_pixels,
            surf_hits, surf_misses, sub_faces_visited, sub_faces_drawn, sub_surf_hits, sub_surf_misses,
            sub_tris, sub_lm_builds, world_pvs_ns, world_sort_ns, world_setup_ns, world_light_ns,
            world_surf_ns, surf_cache_hits, surf_baked, surf_bypass_baked, surf_texels_baked, surf_styled,
            surf_torchlit,
            alias_models, alias_accepted, alias_tris, edges_emitted, surfs_emitted, spans_emitted,
            edges_peak, surfs_peak, bands_ns, band_threads,
        } = *o;
        for (sum, add) in [
            (&mut self.world_ns, world_ns), (&mut self.submodel_ns, submodel_ns),
            (&mut self.external_ns, external_ns), (&mut self.alias_ns, alias_ns),
            (&mut self.particle_ns, particle_ns), (&mut self.sprite_ns, sprite_ns),
            (&mut self.viewmodel_ns, viewmodel_ns), (&mut self.faces_total, faces_total),
            (&mut self.faces_pvs_culled, faces_pvs_culled), (&mut self.faces_frustum_culled, faces_frustum_culled),
            (&mut self.faces_drawn, faces_drawn), (&mut self.world_tris, world_tris),
            (&mut self.world_pixels, world_pixels), (&mut self.surf_hits, surf_hits),
            (&mut self.surf_misses, surf_misses), (&mut self.sub_faces_visited, sub_faces_visited),
            (&mut self.sub_faces_drawn, sub_faces_drawn), (&mut self.sub_surf_hits, sub_surf_hits),
            (&mut self.sub_surf_misses, sub_surf_misses), (&mut self.sub_tris, sub_tris),
            (&mut self.sub_lm_builds, sub_lm_builds), (&mut self.world_pvs_ns, world_pvs_ns),
            (&mut self.world_sort_ns, world_sort_ns), (&mut self.world_setup_ns, world_setup_ns),
            (&mut self.world_light_ns, world_light_ns), (&mut self.world_surf_ns, world_surf_ns),
            (&mut self.surf_cache_hits, surf_cache_hits), (&mut self.surf_baked, surf_baked),
            (&mut self.surf_bypass_baked, surf_bypass_baked), (&mut self.surf_texels_baked, surf_texels_baked),
            (&mut self.surf_styled, surf_styled), (&mut self.surf_torchlit, surf_torchlit),
            (&mut self.alias_models, alias_models), (&mut self.alias_accepted, alias_accepted),
            (&mut self.alias_tris, alias_tris), (&mut self.edges_emitted, edges_emitted),
            (&mut self.surfs_emitted, surfs_emitted), (&mut self.spans_emitted, spans_emitted),
            (&mut self.bands_ns, bands_ns), (&mut self.band_threads, band_threads),
        ] {
            *sum += add;
        }
        self.edges_peak = self.edges_peak.max(edges_peak);
        self.surfs_peak = self.surfs_peak.max(surfs_peak);
    }
}

/// The profiler a [`Renderer`](super::Renderer) owns: the counters while it
/// is on. Its phase timers read `std::time::Instant` (in the browser, WASI's
/// `clock_time_get`: the worker's `performance.now()`).
#[derive(Default)]
pub(super) struct Profiler {
    /// The counters, `Some` while profiling.
    stats: Option<RenderStats>,
}

impl Profiler {
    /// Turn profiling on with every counter at zero.
    pub(super) fn begin(&mut self) {
        self.stats = Some(RenderStats::ZERO);
    }

    /// Turn profiling off and return what it counted (zeros if it was off).
    pub(super) fn end(&mut self) -> RenderStats {
        self.stats.take().unwrap_or(RenderStats::ZERO)
    }

    /// Whether the profiler is counting.
    #[inline]
    pub(super) fn on(&self) -> bool {
        self.stats.is_some()
    }

    /// Apply `f` to the counters iff profiling is on (no-op otherwise).
    #[inline]
    pub(super) fn add(&mut self, f: impl FnOnce(&mut RenderStats)) {
        if let Some(s) = self.stats.as_mut() {
            f(s);
        }
    }

    /// A profiler for a band's passes: on if this one is, its counters at
    /// zero ([`Profiler::absorb`] adds them back).
    pub(super) fn for_band(&self) -> Profiler {
        Profiler { stats: self.stats.map(|_| RenderStats::ZERO) }
    }

    /// Add what a band's profiler ([`Profiler::for_band`]) counted.
    pub(super) fn absorb(&mut self, band: &Profiler) {
        if let (Some(s), Some(b)) = (self.stats.as_mut(), band.stats.as_ref()) {
            s.merge(b);
        }
    }

    /// A timestamp for a phase timer, only while profiling (so the game never
    /// reads a clock).
    #[inline]
    pub(super) fn now(&self) -> Option<std::time::Instant> {
        self.on().then(std::time::Instant::now)
    }
}
