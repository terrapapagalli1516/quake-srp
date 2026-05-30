//! Dynamic lights — Quake's `cl_dlights` pool (`cl_main.c`).
//!
//! Explosions, muzzle flashes, and `EF_*` light effects spawn short-lived point
//! lights that brighten nearby world surfaces. This module ports the client-side
//! allocation/decay bookkeeping (`CL_AllocDlight` / `CL_DecayLights`); the actual
//! per-luxel surface lighting (`R_AddDynamicLights`) lives in [`crate::render`].
//!
//! Faithfulness notes:
//!  * The pool is a fixed array of [`MAX_DLIGHTS`] `= 32` slots, exactly as the C
//!    `cl_dlights[MAX_DLIGHTS]`. No allocation ever grows it, so a runaway spawn
//!    rate cannot grow memory unbounded.
//!  * [`DynamicLights::alloc`] reproduces `CL_AllocDlight`'s three-step slot
//!    policy: reuse the slot whose `key` matches (non-zero key only), else take
//!    the first dead slot (`die < now`), else fall back to slot 0.
//!  * [`DynamicLights::advance`] reproduces `CL_DecayLights`: `radius -= dt*decay`
//!    (clamped at 0). A light is dead once `die < now` or its `radius <= 0`.
//!  * `key == 0` (explosions/temp entities) never matches an existing slot, so it
//!    always takes a fresh dead slot — each explosion gets its own light. A
//!    non-zero `key` (the firing entity's number for muzzle flashes) reuses the
//!    same slot every frame, so the flash tracks the entity instead of filling
//!    the pool.

/// `MAX_DLIGHTS` in `quakedef.h`: the fixed size of the client dynamic-light
/// pool.
pub const MAX_DLIGHTS: usize = 32;

/// One live dynamic light (`dlight_t`). World-space `origin`, current `radius`
/// (Quake light units, decaying toward 0), absolute death time `die`, ambient
/// floor `minlight` (0 for explosions, 32 for muzzle flashes), and per-second
/// `decay` rate. `key` is the owning entity number used by the reuse policy
/// (0 = unowned, e.g. explosions).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DynamicLight {
    /// World-space position of the light.
    pub origin: [f32; 3],
    /// Current radius in Quake light units (decays toward 0 each frame).
    pub radius: f32,
    /// Absolute game time (seconds) at which the light dies.
    pub die: f32,
    /// Ambient floor: a luxel is only lit while the remaining radius exceeds
    /// this. 0 for explosions, 32 for muzzle flashes.
    pub minlight: f32,
    /// Radius decay rate in light units per second (0 = no decay; the light just
    /// lives until `die`).
    pub decay: f32,
    /// Owning entity number for the reuse policy; 0 means unowned.
    key: i32,
}

impl DynamicLight {
    /// Construct a dynamic light directly (the `key` field is private, so this
    /// is how code outside this module — e.g. the renderer's tests — builds a
    /// light). `key` is the owning entity number (0 = unowned).
    pub fn new(
        origin: [f32; 3],
        radius: f32,
        die: f32,
        minlight: f32,
        decay: f32,
        key: i32,
    ) -> DynamicLight {
        DynamicLight {
            origin,
            radius,
            die,
            minlight,
            decay,
            key,
        }
    }

    /// The owning entity number (0 = unowned, e.g. explosions / temp entities).
    pub fn key(&self) -> i32 {
        self.key
    }

    /// A cleared slot with the given key, matching the C `memset(dl,0,..);
    /// dl->key = key`.
    fn cleared(key: i32) -> DynamicLight {
        DynamicLight {
            origin: [0.0; 3],
            radius: 0.0,
            die: 0.0,
            minlight: 0.0,
            decay: 0.0,
            key,
        }
    }
}

/// The client dynamic-light pool: a fixed array of [`MAX_DLIGHTS`] optional
/// slots. `None` is an empty (never-used or freed) slot; `Some` is a light that
/// may still be live or already expired (expiry is resolved in [`Self::advance`]
/// and [`Self::active`]).
#[derive(Debug, Clone)]
pub struct DynamicLights {
    slots: [Option<DynamicLight>; MAX_DLIGHTS],
}

impl Default for DynamicLights {
    fn default() -> DynamicLights {
        DynamicLights::new()
    }
}

impl DynamicLights {
    /// An empty pool (all slots dead).
    pub fn new() -> DynamicLights {
        DynamicLights {
            slots: [None; MAX_DLIGHTS],
        }
    }

    /// `CL_AllocDlight(key)`: choose a slot, clear it, populate it, and return a
    /// reference to the stored light.
    ///
    /// Slot policy (faithful to `cl_main.c`):
    ///  1. If `key != 0`, reuse the first slot whose `key == key` (so a given
    ///     entity reuses its slot every frame instead of filling the pool).
    ///  2. Otherwise take the first dead slot — empty (`None`) or expired
    ///     (`die < now`).
    ///  3. Otherwise fall back to slot 0 (the C overwrites `cl_dlights[0]`).
    ///
    /// The chosen slot is overwritten with `origin`/`radius`/`die`/`decay`/
    /// `minlight` and the given `key`. Never panics and never allocates.
    #[allow(clippy::too_many_arguments)]
    pub fn alloc(
        &mut self,
        key: i32,
        origin: [f32; 3],
        radius: f32,
        die: f32,
        decay: f32,
        minlight: f32,
        now: f32,
    ) -> &mut DynamicLight {
        let idx = self.choose_slot(key, now);
        let mut dl = DynamicLight::cleared(key);
        dl.origin = origin;
        dl.radius = radius;
        dl.die = die;
        dl.decay = decay;
        dl.minlight = minlight;
        self.slots[idx] = Some(dl);
        // `idx < MAX_DLIGHTS` by construction, so the slot is always present.
        self.slots[idx].as_mut().expect("slot just written")
    }

    /// Resolve the slot index per the `CL_AllocDlight` policy. Always in
    /// `0..MAX_DLIGHTS`.
    fn choose_slot(&self, key: i32, now: f32) -> usize {
        // 1. Exact key match (non-zero key only).
        if key != 0 {
            for (i, slot) in self.slots.iter().enumerate() {
                if let Some(dl) = slot {
                    if dl.key == key {
                        return i;
                    }
                }
            }
        }
        // 2. First dead slot (empty or expired).
        for (i, slot) in self.slots.iter().enumerate() {
            let dead = match slot {
                None => true,
                Some(dl) => dl.die < now,
            };
            if dead {
                return i;
            }
        }
        // 3. Fall back to slot 0.
        0
    }

    /// `CL_DecayLights`: decay every live light's radius by `dt * decay`
    /// (clamped at 0), then free any light that has died (`die < now`) or whose
    /// radius has reached 0. `dt` is the frame's elapsed game time and `now` the
    /// current absolute game time.
    ///
    /// A non-finite or negative `dt` is treated as 0 (no decay), so a bad clock
    /// can never resurrect or invert a light.
    pub fn advance(&mut self, dt: f32, now: f32) {
        let dt = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
        for slot in self.slots.iter_mut() {
            let Some(dl) = slot else { continue };
            // Already dead by time: drop it (matches the C `continue` on
            // `die < cl.time`, which leaves it to be reused / never drawn).
            if dl.die < now {
                *slot = None;
                continue;
            }
            // Decay the radius (only when there is a decay rate, as the C only
            // touches live lights; radius==0 lights are also skipped there).
            if dl.radius != 0.0 {
                dl.radius -= dt * dl.decay;
                if dl.radius < 0.0 {
                    dl.radius = 0.0;
                }
            }
            if dl.radius <= 0.0 {
                *slot = None;
            }
        }
    }

    /// The live lights (radius strictly positive). Returned by value so callers
    /// can pass the slice to the renderer without holding a borrow on the pool.
    pub fn active(&self) -> Vec<DynamicLight> {
        self.slots
            .iter()
            .filter_map(|s| *s)
            .filter(|dl| dl.radius > 0.0)
            .collect()
    }

    /// The number of live lights (for diagnostics / playtest reporting).
    pub fn active_count(&self) -> usize {
        self.slots
            .iter()
            .filter_map(|s| s.as_ref())
            .filter(|dl| dl.radius > 0.0)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonzero_key_reuses_same_slot() {
        let mut dl = DynamicLights::new();
        // First alloc with key 7 takes a slot.
        let p0 = dl.alloc(7, [1.0, 2.0, 3.0], 200.0, 1.0, 300.0, 32.0, 0.0) as *const _;
        // A second alloc with the SAME key must reuse the SAME slot (the C
        // reuses the exact key match), so the live count stays 1.
        let p1 = dl.alloc(7, [4.0, 5.0, 6.0], 250.0, 2.0, 300.0, 32.0, 0.0) as *const _;
        assert_eq!(p0, p1, "same key must reuse the same slot");
        assert_eq!(dl.active_count(), 1, "reuse must not create a second light");
        // The reused slot carries the second alloc's data.
        let a = dl.active();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].origin, [4.0, 5.0, 6.0]);
        assert_eq!(a[0].radius, 250.0);
    }

    #[test]
    fn key_zero_takes_fresh_slot_each_time() {
        let mut dl = DynamicLights::new();
        // Explosions pass key 0: each must get its own slot (no reuse).
        dl.alloc(0, [0.0; 3], 350.0, 1.0, 300.0, 0.0, 0.0);
        dl.alloc(0, [0.0; 3], 350.0, 1.0, 300.0, 0.0, 0.0);
        dl.alloc(0, [0.0; 3], 350.0, 1.0, 300.0, 0.0, 0.0);
        assert_eq!(dl.active_count(), 3, "key 0 must allocate fresh slots");
    }

    #[test]
    fn advance_decays_radius() {
        let mut dl = DynamicLights::new();
        dl.alloc(0, [0.0; 3], 350.0, 10.0, 300.0, 0.0, 0.0);
        // 0.5s at decay 300 removes 150 units of radius.
        dl.advance(0.5, 0.5);
        let a = dl.active();
        assert_eq!(a.len(), 1);
        assert!((a[0].radius - 200.0).abs() < 1e-3, "radius should decay by dt*decay");
    }

    #[test]
    fn advance_frees_lights_past_die() {
        let mut dl = DynamicLights::new();
        dl.alloc(0, [0.0; 3], 350.0, 0.5, 0.0, 0.0, 0.0); // die at 0.5, no decay
        // now beyond die: the light must be freed.
        dl.advance(0.1, 1.0);
        assert_eq!(dl.active_count(), 0, "a light past its die time must be freed");
    }

    #[test]
    fn advance_frees_lights_with_zero_radius() {
        let mut dl = DynamicLights::new();
        // radius 100, decay 300: 0.5s overshoots to <=0, so it dies even though
        // die (10.0) is far away.
        dl.alloc(0, [0.0; 3], 100.0, 10.0, 300.0, 0.0, 0.0);
        dl.advance(0.5, 0.5);
        assert_eq!(dl.active_count(), 0, "a light decayed to radius 0 must be freed");
    }

    #[test]
    fn full_pool_falls_back_without_panic() {
        let mut dl = DynamicLights::new();
        // Fill all 32 slots with live, non-expiring lights and distinct keys so
        // no key match and no dead slot exists.
        for i in 0..MAX_DLIGHTS {
            // Non-zero distinct keys (i+1) and a far-future die so none is dead.
            dl.alloc(i as i32 + 1, [i as f32, 0.0, 0.0], 200.0, 1000.0, 0.0, 0.0, 0.0);
        }
        assert_eq!(dl.active_count(), MAX_DLIGHTS);
        // A fresh key-0 alloc with no dead slot must NOT panic; it overwrites
        // slot 0 (the C fallback `&cl_dlights[0]`).
        let before_slot0 = dl.active()[0];
        dl.alloc(0, [999.0, 0.0, 0.0], 350.0, 1.0, 300.0, 0.0, 0.0);
        assert_eq!(dl.active_count(), MAX_DLIGHTS, "fallback must not add a slot");
        // Slot 0 was overwritten with the new light's origin.
        let a = dl.active();
        assert!(
            a.iter().any(|d| d.origin == [999.0, 0.0, 0.0]),
            "fallback must overwrite slot 0 with the new light"
        );
        assert_ne!(before_slot0.origin, [999.0, 0.0, 0.0]);
    }

    #[test]
    fn advance_tolerates_bad_dt() {
        let mut dl = DynamicLights::new();
        dl.alloc(0, [0.0; 3], 200.0, 10.0, 300.0, 0.0, 0.0);
        dl.advance(f32::NAN, 1.0);
        dl.advance(-5.0, 1.0);
        let a = dl.active();
        // No decay applied (bad dt treated as 0), light still alive at 200.
        assert_eq!(a.len(), 1);
        assert!((a[0].radius - 200.0).abs() < 1e-3);
    }
}
