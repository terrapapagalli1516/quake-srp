//! Dynamic lights — Quake's `cl_dlights` pool (`cl_main.c`).
//!
//! Explosions, muzzle flashes, and `EF_*` light effects spawn short-lived point
//! lights that brighten nearby world surfaces. This module ports the client-side
//! bookkeeping — `CL_AllocDlight`, `CL_DecayLights` — and the places the C
//! calls `CL_AllocDlight` from, each as one method both client frames share
//! (the live walk's and the recorded demo's):
//!
//! | method | the C |
//! |---|---|
//! | [`DynamicLights::relink_effects`] | `CL_RelinkEntities`: `EF_MUZZLEFLASH`, `EF_BRIGHTLIGHT`, `EF_DIMLIGHT` |
//! | [`DynamicLights::relink_rocket`] | `CL_RelinkEntities`: a model flagged `EF_ROCKET` |
//! | [`DynamicLights::explosion`] | `CL_ParseTEnt`: `TE_EXPLOSION`, `TE_EXPLOSION2` |
//!
//! The per-luxel surface lighting (`R_AddDynamicLights`) lives in
//! [`crate::render`].
//!
//! Faithfulness notes:
//!  * The pool is a fixed array of [`MAX_DLIGHTS`] `= 32` slots, exactly as the C
//!    `cl_dlights[MAX_DLIGHTS]`: cleared at the start (`CL_ClearState`), never
//!    grown, and a slot is never freed — a dead light stays where it is (die
//!    time passed) until something takes the slot, with its key, as in the C.
//!  * [`DynamicLights::alloc`] reproduces `CL_AllocDlight`'s three-step slot
//!    policy: reuse the slot whose `key` matches (non-zero key only; dead or
//!    alive), else take the first dead slot (`die < now`), else fall back to
//!    slot 0.
//!  * [`DynamicLights::advance`] reproduces `CL_DecayLights`: `radius -= dt*decay`
//!    (clamped at 0) for every light that has not died and has a radius. A light
//!    is drawn (`R_PushDlights`) while `die >= now` and its radius is not 0:
//!    [`DynamicLights::active`].
//!  * The clock is `cl.time` as the C has it, a `double`, and `die` is a `float`:
//!    a light is made with `die = (float)(cl.time + 0.5)` and is dead once
//!    `die < cl.time`, the float widened. In a recorded demo `cl.time` is the
//!    host's running sum, which has more bits than a float, so a light's last
//!    frame — an explosion's, 0.5 s after its message was read, a whole number
//!    of 1/72 s frames on — is decided by those bits; the live walk's clock is
//!    the server's float.
//!  * `key == 0` (explosions/temp entities) never matches an existing slot, so it
//!    always takes a fresh dead slot — each explosion gets its own light. A
//!    non-zero `key` (the entity's number for its effect lights) reuses the
//!    same slot every frame, so the light tracks the entity instead of filling
//!    the pool.

use crate::math::angle_vectors;
use crate::particles::Lcg;
use crate::server::{EF_BRIGHTLIGHT, EF_DIMLIGHT, EF_MUZZLEFLASH};

/// `MAX_DLIGHTS` in `quakedef.h`: the fixed size of the client dynamic-light
/// pool.
pub const MAX_DLIGHTS: usize = 32;

/// One dynamic light (`dlight_t`). World-space `origin`, current `radius`
/// (Quake light units, decaying toward 0), absolute death time `die`, ambient
/// floor `minlight` (0 for explosions, 32 for muzzle flashes), and per-second
/// `decay` rate. `key` is the owning entity number used by the reuse policy
/// (0 = unowned, e.g. explosions). A slot of the pool that holds a light that
/// has died is still a `DynamicLight`, as in the C.
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
    pub fn new(origin: [f32; 3], radius: f32, die: f32, minlight: f32, decay: f32, key: i32) -> DynamicLight {
        DynamicLight { origin, radius, die, minlight, decay, key }
    }

    /// The owning entity number (0 = unowned, e.g. explosions / temp entities).
    pub fn key(&self) -> i32 {
        self.key
    }

    /// A cleared slot with the given key, matching the C `memset(dl,0,..);
    /// dl->key = key`. (`die` 0 is before any `cl.time`, so a cleared slot is
    /// free to take.)
    fn cleared(key: i32) -> DynamicLight {
        DynamicLight { origin: [0.0; 3], radius: 0.0, die: 0.0, minlight: 0.0, decay: 0.0, key }
    }

    /// `R_PushDlights`' test, inverted: the renderer draws a light whose
    /// `die` has not passed and whose radius is not 0.
    fn is_drawn(&self, now: f64) -> bool {
        f64::from(self.die) >= now && self.radius != 0.0
    }
}

/// The client dynamic-light pool: [`MAX_DLIGHTS`] slots, all cleared (`die` 0,
/// no radius) until something takes them, as `CL_ClearState` leaves
/// `cl_dlights`.
#[derive(Debug, Clone)]
pub struct DynamicLights {
    slots: [DynamicLight; MAX_DLIGHTS],
}

impl Default for DynamicLights {
    fn default() -> DynamicLights {
        DynamicLights::new()
    }
}

impl DynamicLights {
    /// An empty pool (all slots cleared).
    pub fn new() -> DynamicLights {
        DynamicLights { slots: [DynamicLight::cleared(0); MAX_DLIGHTS] }
    }

    /// `CL_AllocDlight(key)`: choose a slot, clear it, populate it, and return a
    /// reference to the stored light.
    ///
    /// Slot policy (faithful to `cl_main.c`):
    ///  1. If `key != 0`, reuse the first slot whose `key == key` — dead or
    ///     alive, so an entity gets its own slot back every time it lights
    ///     instead of filling the pool.
    ///  2. Otherwise take the first dead slot (`die < now`; a cleared slot's
    ///     `die` is 0).
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
        now: f64,
    ) -> &mut DynamicLight {
        let idx = self.choose_slot(key, now);
        let dl = &mut self.slots[idx];
        *dl = DynamicLight::cleared(key);
        dl.origin = origin;
        dl.radius = radius;
        dl.die = die;
        dl.decay = decay;
        dl.minlight = minlight;
        dl
    }

    /// Resolve the slot index per the `CL_AllocDlight` policy. Always in
    /// `0..MAX_DLIGHTS`.
    fn choose_slot(&self, key: i32, now: f64) -> usize {
        // 1. Exact key match (non-zero key only), whether the light there is
        //    alive or not.
        if key != 0 {
            if let Some(i) = self.slots.iter().position(|dl| dl.key == key) {
                return i;
            }
        }
        // 2. First dead slot.
        // 3. Otherwise slot 0.
        self.slots.iter().position(|dl| f64::from(dl.die) < now).unwrap_or(0)
    }

    /// `CL_DecayLights`: shrink every light that has not died and has a radius
    /// by `dt * decay` (clamped at 0). `dt` is the frame's `cl.time -
    /// cl.oldtime` and `now` the current `cl.time`. Nothing is freed: a light
    /// whose time has passed, or whose radius has reached 0, is simply no
    /// longer drawn ([`Self::active`]) and its slot is free to take once
    /// `die < now`.
    ///
    /// A non-finite or negative `dt` is treated as 0 (no decay), so a bad clock
    /// can never invert a light.
    pub fn advance(&mut self, dt: f32, now: f64) {
        let dt = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
        for dl in &mut self.slots {
            if f64::from(dl.die) < now || dl.radius == 0.0 {
                continue;
            }
            dl.radius -= dt * dl.decay;
            if dl.radius < 0.0 {
                dl.radius = 0.0;
            }
        }
    }

    /// The lights `R_PushDlights` marks at `now`, with the slot each is in
    /// (its bit in the surfaces' `dlightbits` is `1 << slot`), in slot order:
    /// `die >= now` and a radius.
    pub fn pushed(&self, now: f64) -> impl Iterator<Item = (usize, DynamicLight)> + '_ {
        self.slots.iter().copied().enumerate().filter(move |(_, dl)| dl.is_drawn(now))
    }

    /// The lights drawn at `now` ([`Self::pushed`] without their slots), by
    /// value so callers can pass the slice to the renderer without holding a
    /// borrow on the pool.
    pub fn active(&self, now: f64) -> Vec<DynamicLight> {
        self.pushed(now).map(|(_, dl)| dl).collect()
    }

    /// The number of lights drawn at `now` (for diagnostics / playtest
    /// reporting).
    pub fn active_count(&self, now: f64) -> usize {
        self.pushed(now).count()
    }

    /// `CL_RelinkEntities`' light effects of the entity numbered `key`, at the
    /// `origin` and `angles` it is drawn at and with its `effects` byte, at
    /// client time `now`; `rng` is `rand()` for the radius jitter. Returns
    /// whether the entity discharged a weapon (`EF_MUZZLEFLASH`), which the
    /// client also uses to keep its animation from blending across the flare
    /// ([`crate::client::lerpmodels::FrameLerps::muzzle_flash`]).
    ///
    /// In the C's order, and each as `CL_AllocDlight (i)` with the entity's
    /// number — so an entity with more than one bit set has the one slot, the
    /// last light written winning:
    ///  * `EF_MUZZLEFLASH`: 16 up and 18 along the entity's forward vector
    ///    from its origin, radius `200 + (rand()&31)`, `minlight` 32, for 0.1 s;
    ///  * `EF_BRIGHTLIGHT`: 16 above its origin, radius `400 + (rand()&31)`, for
    ///    0.001 s;
    ///  * `EF_DIMLIGHT`: at its origin, radius `200 + (rand()&31)`, for 0.001 s.
    ///
    /// (The 0.001 s lights live to the end of the frame they are made in and no
    /// longer: the entity makes them again every frame it still has the effect,
    /// a recorded demo's from the message that carried it to the next.)
    pub fn relink_effects(
        &mut self,
        key: i32,
        origin: [f32; 3],
        angles: [f32; 3],
        effects: i32,
        now: f64,
        rng: &mut Lcg,
    ) -> bool {
        let mut jittered = |base: f32| base + rng.next_range(32) as f32;
        let flash = effects & EF_MUZZLEFLASH != 0;
        if flash {
            let (forward, _right, _up) = angle_vectors(angles);
            // `dl->origin[2] += 16`, then `VectorMA (dl->origin, 18, fv, ...)`.
            let raised = [origin[0], origin[1], origin[2] + 16.0];
            let muzzle = std::array::from_fn(|i| raised[i] + 18.0 * forward[i]);
            self.alloc(key, muzzle, jittered(200.0), (now + 0.1) as f32, 0.0, 32.0, now);
        }
        if effects & EF_BRIGHTLIGHT != 0 {
            let up = [origin[0], origin[1], origin[2] + 16.0];
            self.alloc(key, up, jittered(400.0), (now + 0.001) as f32, 0.0, 0.0, now);
        }
        if effects & EF_DIMLIGHT != 0 {
            self.alloc(key, origin, jittered(200.0), (now + 0.001) as f32, 0.0, 0.0, now);
        }
        flash
    }

    /// `CL_RelinkEntities`' light for a model flagged `EF_ROCKET`, on entity
    /// `key` at `origin`: radius 200, for 0.01 s, made again every frame the
    /// rocket flies.
    pub fn relink_rocket(&mut self, key: i32, origin: [f32; 3], now: f64) {
        self.alloc(key, origin, 200.0, (now + 0.01) as f32, 0.0, 0.0, now);
    }

    /// `CL_ParseTEnt`'s light for `TE_EXPLOSION` and `TE_EXPLOSION2` at `origin`:
    /// radius 350 for 0.5 s, shrinking 300 a second (`decay`), unowned (key 0)
    /// so every explosion has a slot of its own. (`TE_TAREXPLOSION` has none.)
    pub fn explosion(&mut self, origin: [f32; 3], now: f64) {
        self.alloc(0, origin, 350.0, (now + 0.5) as f32, 300.0, 0.0, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Slot numbers of the lights drawn at `now`.
    fn slots(dl: &DynamicLights, now: f64) -> Vec<usize> {
        dl.pushed(now).map(|(i, _)| i).collect()
    }

    #[test]
    fn nonzero_key_reuses_same_slot() {
        let mut dl = DynamicLights::new();
        // First alloc with key 7 takes a slot.
        let p0 = dl.alloc(7, [1.0, 2.0, 3.0], 200.0, 2.0, 300.0, 32.0, 1.0) as *const _;
        // A second alloc with the SAME key must reuse the SAME slot (the C
        // reuses the exact key match), so the live count stays 1.
        let p1 = dl.alloc(7, [4.0, 5.0, 6.0], 250.0, 3.0, 300.0, 32.0, 1.0) as *const _;
        assert_eq!(p0, p1, "same key must reuse the same slot");
        assert_eq!(dl.active_count(1.0), 1, "reuse must not create a second light");
        // The reused slot carries the second alloc's data.
        let a = dl.active(1.0);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].origin, [4.0, 5.0, 6.0]);
        assert_eq!(a[0].radius, 250.0);
    }

    #[test]
    fn key_zero_takes_fresh_slot_each_time() {
        let mut dl = DynamicLights::new();
        // Explosions pass key 0: each must get its own slot (no reuse).
        dl.alloc(0, [0.0; 3], 350.0, 2.0, 300.0, 0.0, 1.0);
        dl.alloc(0, [0.0; 3], 350.0, 2.0, 300.0, 0.0, 1.0);
        dl.alloc(0, [0.0; 3], 350.0, 2.0, 300.0, 0.0, 1.0);
        assert_eq!(dl.active_count(1.0), 3, "key 0 must allocate fresh slots");
    }

    /// `CL_AllocDlight`'s three steps, slot by slot: the slot index is part of
    /// id's state (`1 << slot` marks the surfaces), so the oracle compares it.
    #[test]
    fn alloc_picks_the_slot_id_s_does() {
        let mut dl = DynamicLights::new();
        // Free slots are taken in order, key or not.
        dl.alloc(0, [10.0, 0.0, 0.0], 100.0, 5.0, 0.0, 0.0, 1.0);
        dl.alloc(7, [11.0, 0.0, 0.0], 100.0, 5.0, 0.0, 0.0, 1.0);
        dl.alloc(0, [12.0, 0.0, 0.0], 100.0, 5.0, 0.0, 0.0, 1.0);
        let lights: Vec<_> = dl.pushed(1.0).map(|(i, l)| (i, l.key(), l.origin[0])).collect();
        assert_eq!(lights, vec![(0, 0, 10.0), (1, 7, 11.0), (2, 0, 12.0)]);
        // A key is found where it is, not in the first free slot (3).
        dl.alloc(7, [13.0, 0.0, 0.0], 100.0, 5.0, 0.0, 0.0, 1.0);
        let lights: Vec<_> = dl.pushed(1.0).map(|(i, l)| (i, l.origin[0])).collect();
        assert_eq!(lights, vec![(0, 10.0), (1, 13.0), (2, 12.0)]);
        // The slot is still the key's after its light has died: at 6.0 all
        // three are dead, and key 7 wins over the first dead slot, 0.
        dl.alloc(7, [14.0, 0.0, 0.0], 100.0, 9.0, 0.0, 0.0, 6.0);
        assert_eq!(slots(&dl, 6.0), vec![1]);
        // An unkeyed light then takes the first dead slot: 0.
        dl.alloc(0, [15.0, 0.0, 0.0], 100.0, 9.0, 0.0, 0.0, 6.0);
        assert_eq!(slots(&dl, 6.0), vec![0, 1]);
        // A key that has no slot takes the first dead one, 2, and keeps it.
        dl.alloc(8, [16.0, 0.0, 0.0], 100.0, 9.0, 0.0, 0.0, 6.0);
        assert_eq!(slots(&dl, 6.0), vec![0, 1, 2]);
        dl.alloc(8, [17.0, 0.0, 0.0], 100.0, 9.0, 0.0, 0.0, 6.5);
        assert_eq!(slots(&dl, 6.5), vec![0, 1, 2]);
    }

    /// A slot is free only once `die < now` (a light at its die time is still
    /// alive), and a light decayed to no radius but not yet dead is not free
    /// to take either, as in the C.
    #[test]
    fn a_slot_is_free_only_once_die_has_passed() {
        let mut dl = DynamicLights::new();
        dl.alloc(0, [1.0, 0.0, 0.0], 100.0, 2.0, 300.0, 0.0, 1.0);
        dl.alloc(0, [2.0, 0.0, 0.0], 100.0, 2.0, 300.0, 0.0, 1.0);
        // At now = die the first is alive: the next alloc must not take slot 0.
        dl.alloc(0, [3.0, 0.0, 0.0], 100.0, 9.0, 0.0, 0.0, 2.0);
        assert_eq!(slots(&dl, 2.0), vec![0, 1, 2]);
        // Decayed to nothing but not past die: not drawn, not free.
        dl.advance(1.0, 2.0);
        assert_eq!(slots(&dl, 2.0), vec![2], "the two decayed lights are not drawn");
        dl.alloc(0, [4.0, 0.0, 0.0], 100.0, 9.0, 0.0, 0.0, 2.0);
        assert_eq!(slots(&dl, 2.0), vec![2, 3]);
        // Past die the slot is taken, first dead one first.
        dl.alloc(0, [5.0, 0.0, 0.0], 100.0, 9.0, 0.0, 0.0, 2.5);
        assert_eq!(slots(&dl, 2.5), vec![0, 2, 3]);
    }

    #[test]
    fn advance_decays_radius() {
        let mut dl = DynamicLights::new();
        dl.alloc(0, [0.0; 3], 350.0, 10.0, 300.0, 0.0, 1.0);
        // 0.5s at decay 300 removes 150 units of radius.
        dl.advance(0.5, 1.5);
        let a = dl.active(1.5);
        assert_eq!(a.len(), 1);
        assert!((a[0].radius - 200.0).abs() < 1e-3, "radius should decay by dt*decay");
    }

    #[test]
    fn a_light_past_die_is_not_drawn_nor_decayed() {
        let mut dl = DynamicLights::new();
        dl.alloc(0, [0.0; 3], 350.0, 1.5, 300.0, 0.0, 1.0); // die at 1.5
        // Alive at its die time, gone after it; the C's `die < cl.time` skip
        // leaves the radius alone.
        assert_eq!(dl.active_count(1.5), 1);
        dl.advance(0.1, 2.0);
        assert_eq!(dl.active_count(2.0), 0, "a light past its die time is not drawn");
        assert_eq!(dl.slots[0].radius, 350.0, "and CL_DecayLights does not touch it");
    }

    #[test]
    fn a_light_decayed_to_zero_radius_is_not_drawn() {
        let mut dl = DynamicLights::new();
        // radius 100, decay 300: 0.5s overshoots to <=0, so it is clamped at 0
        // and not drawn even though die (10.0) is far away.
        dl.alloc(0, [0.0; 3], 100.0, 10.0, 300.0, 0.0, 1.0);
        dl.advance(0.5, 1.5);
        assert_eq!(dl.slots[0].radius, 0.0, "clamped, never negative");
        assert_eq!(dl.active_count(1.5), 0, "a light decayed to radius 0 is not drawn");
    }

    #[test]
    fn full_pool_falls_back_without_panic() {
        let mut dl = DynamicLights::new();
        // Fill all 32 slots with live, non-expiring lights and distinct keys so
        // no key match and no dead slot exists.
        for i in 0..MAX_DLIGHTS {
            // Non-zero distinct keys (i+1) and a far-future die so none is dead.
            dl.alloc(i as i32 + 1, [i as f32, 0.0, 0.0], 200.0, 1000.0, 0.0, 0.0, 1.0);
        }
        assert_eq!(dl.active_count(1.0), MAX_DLIGHTS);
        // A fresh key-0 alloc with no dead slot must NOT panic; it overwrites
        // slot 0 (the C fallback `&cl_dlights[0]`).
        let before_slot0 = dl.slots[0];
        dl.alloc(0, [999.0, 0.0, 0.0], 350.0, 2.0, 300.0, 0.0, 1.0);
        assert_eq!(dl.active_count(1.0), MAX_DLIGHTS, "fallback must not add a slot");
        // Slot 0 was overwritten with the new light, key and all.
        assert_eq!(dl.slots[0].origin, [999.0, 0.0, 0.0]);
        assert_eq!(dl.slots[0].key(), 0);
        assert_ne!(before_slot0.origin, [999.0, 0.0, 0.0]);
        // The next one lands on slot 0 again, the others untouched.
        dl.alloc(0, [998.0, 0.0, 0.0], 350.0, 2.0, 300.0, 0.0, 1.0);
        assert_eq!(dl.slots[0].origin, [998.0, 0.0, 0.0]);
        assert_eq!(dl.slots[1].origin, [1.0, 0.0, 0.0]);
    }

    #[test]
    fn advance_tolerates_bad_dt() {
        let mut dl = DynamicLights::new();
        dl.alloc(0, [0.0; 3], 200.0, 10.0, 300.0, 0.0, 1.0);
        dl.advance(f32::NAN, 1.0);
        dl.advance(-5.0, 1.0);
        let a = dl.active(1.0);
        // No decay applied (bad dt treated as 0), light still alive at 200.
        assert_eq!(a.len(), 1);
        assert!((a[0].radius - 200.0).abs() < 1e-3);
    }

    /// A light's radius less its base: what `rand()&31` added.
    fn jitter(radius: f32, base: f32) -> f32 {
        radius - base
    }

    #[test]
    fn muzzle_flash_is_16_up_and_18_forward_with_jitter_and_a_min_light() {
        let mut dl = DynamicLights::new();
        let mut rng = Lcg::new(1);
        // Facing +x: forward = [1, 0, 0].
        let flashed = dl.relink_effects(5, [100.0, 200.0, 50.0], [0.0; 3], EF_MUZZLEFLASH, 3.0, &mut rng);
        assert!(flashed, "the caller keys the animation snap on it");
        let (slot, l) = dl.pushed(3.0).next().expect("one light");
        assert_eq!(slot, 0);
        assert_eq!(l.key(), 5, "keyed to the entity");
        assert_eq!(l.minlight, 32.0);
        assert_eq!(l.decay, 0.0);
        assert!((l.die - 3.1).abs() < 1e-6, "die = cl.time + 0.1");
        assert!((0.0..32.0).contains(&jitter(l.radius, 200.0)), "200 + (rand()&31): {}", l.radius);
        // origin + [0,0,16] + 18 * [1,0,0]
        assert_eq!(l.origin, [118.0, 200.0, 66.0]);
        // Pitched 90 degrees down: forward = [0, 0, -1].
        dl.relink_effects(6, [0.0, 0.0, 100.0], [90.0, 0.0, 0.0], EF_MUZZLEFLASH, 3.0, &mut rng);
        let down = dl.pushed(3.0).find(|(_, l)| l.key() == 6).unwrap().1;
        assert!((down.origin[2] - (100.0 + 16.0 - 18.0)).abs() < 1e-4, "{:?}", down.origin);
        // Facing +y: forward comes from AngleVectors, as the C's.
        let (fwd, _, _) = angle_vectors([0.0, 90.0, 0.0]);
        dl.relink_effects(7, [0.0; 3], [0.0, 90.0, 0.0], EF_MUZZLEFLASH, 3.0, &mut rng);
        let yaw = dl.pushed(3.0).find(|(_, l)| l.key() == 7).unwrap().1;
        assert_eq!(yaw.origin, [18.0 * fwd[0], 18.0 * fwd[1], 16.0 + 18.0 * fwd[2]]);
    }

    #[test]
    fn bright_and_dim_light_effects() {
        let mut dl = DynamicLights::new();
        let mut rng = Lcg::new(2);
        let flashed = dl.relink_effects(8, [10.0, 20.0, 30.0], [0.0; 3], EF_BRIGHTLIGHT, 2.0, &mut rng);
        assert!(!flashed, "only the muzzle flash is a flash");
        let b = dl.pushed(2.0).find(|(_, l)| l.key() == 8).unwrap().1;
        assert_eq!(b.origin, [10.0, 20.0, 46.0], "16 above the origin");
        assert!((0.0..32.0).contains(&jitter(b.radius, 400.0)), "400 + (rand()&31): {}", b.radius);
        assert_eq!(b.minlight, 0.0);
        assert!((b.die - 2.001).abs() < 1e-6, "die = cl.time + 0.001");
        dl.relink_effects(9, [40.0, 50.0, 60.0], [0.0; 3], EF_DIMLIGHT, 2.0, &mut rng);
        let d = dl.pushed(2.0).find(|(_, l)| l.key() == 9).unwrap().1;
        assert_eq!(d.origin, [40.0, 50.0, 60.0], "at the origin");
        assert!((0.0..32.0).contains(&jitter(d.radius, 200.0)), "200 + (rand()&31): {}", d.radius);
        assert!((d.die - 2.001).abs() < 1e-6);
        // Past its 0.001 s the lights are not drawn: they live for the frame
        // they were made in.
        assert_eq!(dl.pushed(2.01).count(), 0);
    }

    #[test]
    fn an_entity_with_several_effects_has_the_one_slot_the_last_wins() {
        let mut dl = DynamicLights::new();
        let mut rng = Lcg::new(3);
        let all = EF_MUZZLEFLASH | EF_BRIGHTLIGHT | EF_DIMLIGHT;
        assert!(dl.relink_effects(4, [0.0, 0.0, 0.0], [0.0; 3], all, 1.0, &mut rng));
        let lights: Vec<_> = dl.pushed(1.0).collect();
        assert_eq!(lights.len(), 1, "CL_AllocDlight (i) three times is one slot");
        // The dim light is written last: at the origin, no minlight.
        assert_eq!(lights[0].1.origin, [0.0; 3]);
        assert_eq!(lights[0].1.minlight, 0.0);
        // No effects bit, no light, no flash.
        let mut none = DynamicLights::new();
        assert!(!none.relink_effects(4, [0.0; 3], [0.0; 3], 0, 1.0, &mut rng));
        assert_eq!(none.pushed(1.0).count(), 0);
        // One draw of the generator per light made, as the C's one rand().
        let (mut a, mut b) = (Lcg::new(3), Lcg::new(3));
        DynamicLights::new().relink_effects(4, [0.0; 3], [0.0; 3], all, 1.0, &mut a);
        for _ in 0..3 {
            b.next_range(32);
        }
        assert_eq!(a.next_range(1000), b.next_range(1000));
    }

    #[test]
    fn rocket_and_explosion_lights() {
        let mut dl = DynamicLights::new();
        dl.relink_rocket(12, [1.0, 2.0, 3.0], 4.0);
        let r = dl.pushed(4.0).next().unwrap().1;
        assert_eq!((r.key(), r.origin, r.radius, r.minlight, r.decay), (12, [1.0, 2.0, 3.0], 200.0, 0.0, 0.0));
        assert!((r.die - 4.01).abs() < 1e-5, "die = cl.time + 0.01");
        // Made again next frame, it is the same slot.
        dl.relink_rocket(12, [2.0, 2.0, 3.0], 4.014);
        assert_eq!(dl.pushed(4.014).map(|(i, l)| (i, l.origin[0])).collect::<Vec<_>>(), vec![(0, 2.0)]);
        // An explosion: 350, 0.5 s, shrinking 300 a second, unowned.
        dl.explosion([9.0, 8.0, 7.0], 5.0);
        let e = dl.pushed(5.0).find(|(_, l)| l.key() == 0).unwrap().1;
        assert_eq!((e.origin, e.radius, e.minlight, e.decay), ([9.0, 8.0, 7.0], 350.0, 0.0, 300.0));
        assert!((e.die - 5.5).abs() < 1e-6);
        dl.advance(0.1, 5.1);
        let e = dl.pushed(5.1).find(|(_, l)| l.key() == 0).unwrap().1;
        assert!((e.radius - 320.0).abs() < 1e-3);
        // A second explosion does not take the first one's slot.
        dl.explosion([0.0; 3], 5.1);
        assert_eq!(dl.pushed(5.1).filter(|(_, l)| l.key() == 0).count(), 2);
    }
}
