//! EXTRA, not id (2026, off in Classic): the steady torches flicker
//! (`r_torchflicker`).
//!
//! id's mappers gave a few wall torches an animated light style — `start`'s
//! and episode 2's, world.qc's two flickers, styles 1 and 6 — and left the
//! rest steady (style 0): all 143 torches and flames of e1m2–e1m7, e2m6's
//! 109. LIGHT.EXE baked a steady torch into each face's style-0 lightmap with
//! the rest of the room's light, so nothing at run time knows it is there.
//! This module finds it again. From the entity lump, each steady torch's
//! origin and `light`; for each face it reaches, its share of each luxel as
//! the light tool computed it ([`share`]: `(light - dist)` times the tool's
//! angle and range scales, without the tool's shadows, never more than the
//! luxel holds). Each frame moves that share by the torch's flicker:
//!
//! ```text
//! luxel(t) = id's luxel + strength · (s(t)/s̄ - 1) · share · d_lightstylevalue[0] / 256
//! ```
//!
//! `s(t)` is one of world.qc's flicker patterns through the light-style glide
//! ([`crate::server::LerpLightStyles`], stepped as id's when it is off), each
//! torch at its own phase from its origin so a row of torches never pulses as
//! one, and `s̄` the pattern's mean, so the change is zero-mean: over the
//! pattern's period every luxel averages to id's, and the level is exactly as
//! dark as id made it, only alive. It is the light the torch would have given
//! had the mapper set its style to the flicker (what `start`'s torches do),
//! brought back to the steady torch's average.
//!
//! The faces a torch reaches are rebaked when its value moves, as an animated
//! style's are: the lightmap and lit-surface caches key a face on its torches'
//! scales ([`FaceTorches::key_is`]), and the glide moves in whole steps
//! (`server::GLIDE_STEP`), so a block rebakes at most once a step. A torch with
//! an animated style of its own is left to it (no double flicker); one QuakeC
//! switches (style 32 and up) is left alone.
//!
//! Alias models (the monsters, the gun) are lit by the luxel under them
//! (`R_LightPoint`), and pick up that luxel's change ([`FaceTorches::at`]).

use crate::bsp::{Bsp, DFace, TEX_SPECIAL};
use crate::math::{dot, Vec3};
use crate::server::{lightstyle_value_at, LerpLightStyles, Tokenizer};

use super::light::{surface_extents, STYLE_NONE};
use super::surf::face_world_poly;

/// `r_torchflicker`: how much the steady torches flicker, in hundredths of
/// the flicker style's own swing — 0 off (id's: Classic), 100 as if the
/// mapper had given each torch its flicker style, at most 200.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TorchFlicker(u8);

impl TorchFlicker {
    /// id's: the torches as LIGHT.EXE baked them.
    pub const OFF: TorchFlicker = TorchFlicker(0);
    /// The flicker style's own swing (the cvar's 1).
    pub const STYLE: TorchFlicker = TorchFlicker(100);
    /// The 2026 profile's.
    pub const MODERN: TorchFlicker = TorchFlicker::STYLE;
    /// The largest strength the cvar takes.
    pub const MAX: f32 = 2.0;

    /// The cvar's value: clamped to `0..=2` and rounded to a hundredth; not a
    /// number is off.
    #[must_use]
    pub fn from_value(v: f32) -> TorchFlicker {
        if v > 0.0 {
            TorchFlicker((v.min(Self::MAX) * 100.0).round() as u8)
        } else {
            TorchFlicker::OFF
        }
    }

    /// The strength as the cvar reads it (1.0 the flicker style's swing).
    #[must_use]
    pub fn value(self) -> f32 {
        f32::from(self.0) / 100.0
    }

    #[must_use]
    pub fn is_off(self) -> bool {
        self.0 == 0
    }
}

// ---------------------------------------------------------------------------
// The flames and their flicker
// ---------------------------------------------------------------------------

/// world.qc's FLICKER (first variety), light style 1.
const FLICKER_1: &[u8] = b"mmnmmommommnonmmonqnmmo";
/// world.qc's FLICKER (second variety), light style 6.
const FLICKER_6: &[u8] = b"nmonqnmomnmomomno";

/// How a kind of flame flickers: one of world.qc's flicker patterns, read at
/// `rate` letters a tenth of a second (id's styles: 1), its swing about its
/// mean scaled by `depth`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Flicker {
    pattern: &'static [u8],
    rate: f32,
    depth: f32,
}

/// The three kinds of flame the shareware and registered maps place
/// (misc.qc's spawn functions; `light_flame_small_white` is the yellow one's
/// white twin).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Flame {
    /// `light_torch_small_walltorch` (`progs/flame.mdl`): the mapper's choice
    /// for a flickering torch, where id made one (styles 1 and 6).
    WallTorch,
    /// `light_flame_large_yellow` (`progs/flame2.mdl` frame 1): the big brazier
    /// fire.
    LargeFlame,
    /// `light_flame_small_yellow`, `light_flame_small_white` (`flame2.mdl`
    /// frame 0): the small flame on a stand.
    SmallFlame,
}

impl Flame {
    fn of_classname(name: &str) -> Option<Flame> {
        match name {
            "light_torch_small_walltorch" => Some(Flame::WallTorch),
            "light_flame_large_yellow" => Some(Flame::LargeFlame),
            "light_flame_small_yellow" | "light_flame_small_white" => Some(Flame::SmallFlame),
            _ => None,
        }
    }

    /// The kind's flicker.
    pub(super) fn flicker(self) -> Flicker {
        match self {
            Flame::WallTorch => Flicker { pattern: FLICKER_6, rate: 1.0, depth: 1.0 },
            Flame::LargeFlame => Flicker { pattern: FLICKER_1, rate: 1.0, depth: 1.0 },
            Flame::SmallFlame => Flicker { pattern: FLICKER_1, rate: 1.0, depth: 1.0 },
        }
    }
}

/// `pattern`'s mean light value (`(letter - 'a') * 22` over its letters): the
/// level a flickering light averages over its period, stepped or gliding.
fn pattern_mean(pattern: &[u8]) -> f32 {
    let sum: i32 = pattern.iter().map(|&c| i32::from(c.wrapping_sub(b'a') % 26) * 22).sum();
    sum as f32 / pattern.len().max(1) as f32
}

/// A torch's phase in its pattern, in letters, from its origin: a hash of the
/// whole units the entity lump gives, so it is the same every time the map
/// loads, in the live game and in a demo, and neighbouring torches land far
/// apart. Sixteenths of a letter, so torches do not even step together.
fn phase_of(origin: Vec3, len: usize) -> f32 {
    let mut h: u32 = 0x811c_9dc5;
    for c in origin {
        h = (h ^ (c.round() as i32 as u32)).wrapping_mul(0x0100_0193);
    }
    // murmur3's finaliser: every input bit reaches every output bit.
    h ^= h >> 16;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    let sixteenths = (len.max(1) as u32) * 16;
    (h % sixteenths) as f32 / 16.0
}

/// One steady torch from the entity lump.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Torch {
    pub(super) origin: Vec3,
    /// The `light` key, or LIGHT.EXE's `DEFAULTLIGHTLEVEL` (300) without one.
    pub(super) light: f32,
    pub(super) flame: Flame,
    /// Where in its pattern it is at time 0, in letters ([`phase_of`]).
    phase: f32,
    /// The pattern's mean ([`pattern_mean`]).
    mean: f32,
}

impl Torch {
    /// The torch's scale this frame: `strength · depth · (s(t)/s̄ - 1)`, the
    /// fraction of its share each luxel it lights gains (or loses) at `time`
    /// (`cl.time`). A pure function of the time and the torch.
    pub(super) fn scale(&self, time: f32, lerp: LerpLightStyles, strength: TorchFlicker) -> f32 {
        if strength.is_off() {
            return 0.0;
        }
        let f = self.flame.flicker();
        let letters = f64::from(if time.is_finite() { time } else { 0.0 }) * 10.0 * f64::from(f.rate) + f64::from(self.phase);
        let v = lightstyle_value_at(f.pattern, letters, lerp);
        strength.value() * f.depth * (v as f32 / self.mean - 1.0)
    }
}

/// LIGHT.EXE's `DEFAULTLIGHTLEVEL` (`light.h`): a `light*` entity without a
/// `light` key (or with 0) shines at 300, the torches too (misc.qc's "Default
/// light value is 200" is the editor's note, not the tool's).
const DEFAULT_LIGHT: f32 = 300.0;

/// The steady torches in an entity lump: every flame class with no `style`
/// (or style 0). A torch with an animated style is the light style's to
/// animate; one with a switched style (32 and up) is QuakeC's.
pub(super) fn steady_torches(entities: &str) -> Vec<Torch> {
    let mut out = Vec::new();
    let mut tok = Tokenizer::new(entities);
    while let Some(open) = tok.next_token() {
        if open != "{" {
            break;
        }
        let (mut class, mut origin, mut light, mut style) = (String::new(), None, 0.0f32, 0i32);
        while let Some(key) = tok.next_token() {
            if key == "}" {
                break;
            }
            let Some(value) = tok.next_token() else { break };
            match key.as_str() {
                "classname" => class = value,
                "origin" => origin = Some(crate::server::parse_vector(&value)),
                "light" => light = crate::server::parse_float(&value),
                "style" => style = crate::server::parse_int(&value),
                _ => {}
            }
        }
        let (Some(flame), Some(origin)) = (Flame::of_classname(&class), origin) else { continue };
        if style != 0 {
            continue;
        }
        let light = if light == 0.0 { DEFAULT_LIGHT } else { light };
        let pattern = flame.flicker().pattern;
        out.push(Torch { origin, light, flame, phase: phase_of(origin, pattern.len()), mean: pattern_mean(pattern) });
    }
    out
}

// ---------------------------------------------------------------------------
// What each torch gave each luxel
// ---------------------------------------------------------------------------

/// LIGHT.EXE's `scalecos` (0.5): half of a luxel's light falls off with the
/// angle it arrives at, half does not.
const SCALECOS: f32 = 0.5;
/// LIGHT.EXE's `rangescale` (0.5): the light sum is halved before it is
/// clamped to a byte.
const RANGESCALE: f32 = 0.5;
/// A share smaller than this on every luxel is not kept (the tool ignores
/// "real tiny lights" too).
const MIN_SHARE: f32 = 0.5;

/// `SingleLightFace`'s add for one sample point, times the tool's
/// `rangescale`: what a torch of `light` at `origin` gave the luxel whose
/// sample point is `p` on a face facing `normal` — `(light - dist)` scaled by
/// `(1 - scalecos) + scalecos * cos(angle)`, nothing past `light` — without
/// `CastRay`'s shadow test: the dumb robust option, as id's dynamic lights
/// also shine through walls; [`TorchSet::build`] bounds the shares by what the
/// luxel holds, so a torch never lights more than the tool did.
pub(super) fn share(origin: Vec3, light: f32, p: Vec3, normal: Vec3) -> f32 {
    let d = [origin[0] - p[0], origin[1] - p[1], origin[2] - p[2]];
    let dist = dot(d, d).sqrt();
    if !(dist < light) {
        return 0.0;
    }
    let cos = if dist > 0.0 { dot(d, normal) / dist } else { 1.0 };
    let angle = (1.0 - SCALECOS) + SCALECOS * cos;
    ((light - dist) * angle).max(0.0) * RANGESCALE
}

/// One torch's shares of one face's luxels (the face's `lmw * lmh` grid, in
/// the style-0 block's luxel units).
struct TorchLit {
    torch: u32,
    shares: Box<[f32]>,
}

/// The map's steady torches, what each gave each face, and their scales this
/// frame: built from the world's entity lump the first frame the extra is on
/// ([`TorchSet::build`]), animated every frame ([`TorchSet::animate`]).
pub(super) struct TorchSet {
    torches: Vec<Torch>,
    /// Per world face (`bsp.faces`), its range of `lit`.
    faces: Vec<(u32, u32)>,
    lit: Vec<TorchLit>,
    /// Each torch's scale this frame ([`Torch::scale`]).
    scales: Vec<f32>,
}

impl TorchSet {
    /// The steady torches of `world`'s entity lump and, for every face of the
    /// world and its brush models one reaches, its shares of the face's
    /// luxels, as the light tool lit it: the faces it is in front of
    /// (`SingleLightFace`: `dist <= 0` or `dist > light` skip the face), at
    /// the tool's sample points (`CalcPoints`: each luxel's texture
    /// coordinates on the face's plane, a unit in front of it). The shares of
    /// a luxel are bounded by the luxel its style-0 block holds — a torch the
    /// tool found shadowed, or whose light another clamped, gives no more
    /// than is there, and a face without a style-0 block gets nothing.
    pub(super) fn build(world: &Bsp) -> TorchSet {
        let torches = steady_torches(&world.entities);
        let mut faces = vec![(0u32, 0u32); world.faces.len()];
        let mut lit = Vec::new();
        if !torches.is_empty() && !world.lighting.is_empty() {
            let mut poly = Vec::new();
            for (fi, face) in world.faces.iter().enumerate() {
                let start = lit.len() as u32;
                face_shares(world, face, &torches, &mut poly, &mut lit);
                faces[fi] = (start, lit.len() as u32);
            }
        }
        let scales = vec![0.0; torches.len()];
        TorchSet { torches, faces, lit, scales }
    }

    /// Set every torch's scale for the frame at `time` (`cl.time`).
    pub(super) fn animate(&mut self, time: f32, lerp: LerpLightStyles, strength: TorchFlicker) {
        for (s, t) in self.scales.iter_mut().zip(&self.torches) {
            *s = t.scale(time, lerp, strength);
        }
    }

    /// The torches lighting world face `fi`, at this frame's scales.
    pub(super) fn face(&self, fi: usize) -> FaceTorches<'_> {
        let (a, b) = self.faces.get(fi).copied().unwrap_or((0, 0));
        FaceTorches { lit: &self.lit[a as usize..b as usize], scales: &self.scales }
    }

    /// The steady torches found, and the (face, torch) pairs that light.
    #[cfg(test)]
    pub(super) fn counts(&self) -> (usize, usize) {
        (self.torches.len(), self.lit.len())
    }

    #[cfg(test)]
    pub(super) fn torches(&self) -> &[Torch] {
        &self.torches
    }
}

/// [`TorchSet::build`] for one face: push a [`TorchLit`] onto `lit` for each
/// torch that lights it.
fn face_shares(bsp: &Bsp, face: &DFace, torches: &[Torch], poly: &mut Vec<Vec3>, lit: &mut Vec<TorchLit>) {
    let Some(ti) = usize::try_from(face.texinfo).ok().and_then(|i| bsp.texinfo.get(i)) else { return };
    if ti.flags & TEX_SPECIAL != 0 || face.lightofs < 0 {
        return;
    }
    // The torch's light is in the face's style-0 block.
    let Some(slot) = face.styles.iter().take_while(|&&s| s != STYLE_NONE).position(|&s| s == 0) else { return };
    let Some(plane) = usize::try_from(face.planenum).ok().and_then(|i| bsp.planes.get(i)) else { return };
    // `l->facenormal`, `l->facedist`: the plane turned to the face's side.
    let (normal, facedist) = if face.side != 0 {
        ([-plane.normal[0], -plane.normal[1], -plane.normal[2]], -plane.dist)
    } else {
        (plane.normal, plane.dist)
    };
    let reaching: Vec<u32> = (0..torches.len() as u32)
        .filter(|&i| {
            let t = &torches[i as usize];
            let d = dot(t.origin, normal) - facedist;
            d > 0.0 && d <= t.light
        })
        .collect();
    if reaching.is_empty() || !face_world_poly(bsp, face, poly) {
        return;
    }
    let Some((texmins, extent)) = surface_extents(ti, poly) else { return };
    let (lmw, lmh) = ((extent[0] / 16 + 1) as usize, (extent[1] / 16 + 1) as usize);
    let n = lmw * lmh;
    let Some(block) = usize::try_from(face.lightofs)
        .ok()
        .and_then(|o| o.checked_add(slot * n))
        .and_then(|o| bsp.lighting.get(o..o.checked_add(n)?))
    else {
        return;
    };
    // The sample point of each luxel: on the plane a unit in front
    // (`CalcFaceVectors`' texorg), at the texture coordinates
    // `texturemins + 16 * (s, t)` (`CalcPoints` without `-extra`): solve
    // `vecs[0]·p + vecs[0][3] = s`, `vecs[1]·p + vecs[1][3] = t`,
    // `normal·p = facedist + 1` for p.
    let rows = [
        [ti.vecs[0][0], ti.vecs[0][1], ti.vecs[0][2]],
        [ti.vecs[1][0], ti.vecs[1][1], ti.vecs[1][2]],
        normal,
    ];
    let Some(inv) = invert3(rows) else { return };
    let point = |s: usize, t: usize| {
        let b = [
            (texmins[0] + 16 * s as i32) as f32 - ti.vecs[0][3],
            (texmins[1] + 16 * t as i32) as f32 - ti.vecs[1][3],
            facedist + 1.0,
        ];
        [dot(inv[0], b), dot(inv[1], b), dot(inv[2], b)]
    };
    let first = lit.len();
    for &i in &reaching {
        let t = &torches[i as usize];
        let mut shares = vec![0.0f32; n];
        for (j, cell) in shares.iter_mut().enumerate() {
            *cell = share(t.origin, t.light, point(j % lmw, j / lmw), normal);
        }
        if shares.iter().any(|&s| s >= MIN_SHARE) {
            lit.push(TorchLit { torch: i, shares: shares.into_boxed_slice() });
        }
    }
    // Bound each luxel's shares by what it holds.
    let mine = &mut lit[first..];
    for (j, &held) in block.iter().enumerate() {
        let total: f32 = mine.iter().map(|l| l.shares[j]).sum();
        let held = f32::from(held);
        if total > held {
            let k = if total > 0.0 { held / total } else { 0.0 };
            for l in mine.iter_mut() {
                l.shares[j] *= k;
            }
        }
    }
}

/// The inverse of the 3x3 matrix `m` (rows), or `None` when it is singular
/// (a texture axis along the face's normal: the tool's "Texture axis
/// perpendicular to face").
fn invert3(m: [Vec3; 3]) -> Option<[Vec3; 3]> {
    let c = |a: Vec3, b: Vec3| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let (r0, r1, r2) = (c(m[1], m[2]), c(m[2], m[0]), c(m[0], m[1]));
    let det = dot(m[0], r0);
    if !(det.abs() > 1e-9) {
        return None;
    }
    // The inverse's columns are the cofactor rows over the determinant.
    let k = 1.0 / det;
    Some([
        [r0[0] * k, r1[0] * k, r2[0] * k],
        [r0[1] * k, r1[1] * k, r2[1] * k],
        [r0[2] * k, r1[2] * k, r2[2] * k],
    ])
}

/// The torches lighting one face, at this frame's scales
/// ([`TorchSet::face`]); [`FaceTorches::NONE`] where there are none, the
/// extra is off, or the face is not the world's.
#[derive(Clone, Copy)]
pub(super) struct FaceTorches<'a> {
    lit: &'a [TorchLit],
    scales: &'a [f32],
}

impl FaceTorches<'_> {
    /// No torch: id's light.
    pub(super) const NONE: FaceTorches<'static> = FaceTorches { lit: &[], scales: &[] };

    /// No torch lights the face.
    pub(super) fn is_empty(&self) -> bool {
        self.lit.is_empty()
    }

    fn scale(&self, l: &TorchLit) -> f32 {
        self.scales.get(l.torch as usize).copied().unwrap_or(0.0)
    }

    /// The caches' key: the face's torches' scales, in order.
    pub(super) fn key(&self) -> Box<[f32]> {
        self.lit.iter().map(|l| self.scale(l)).collect()
    }

    /// Whether `key` ([`FaceTorches::key`]) is this frame's, bit for bit.
    pub(super) fn key_is(&self, key: &[f32]) -> bool {
        key.len() == self.lit.len() && self.lit.iter().zip(key).all(|(l, k)| self.scale(l).to_bits() == k.to_bits())
    }

    /// Add the torches' change to a face's combined luxels (`R_BuildLightMap`'s
    /// sum over 256, `lmw * lmh`): each torch's scale times its share, times
    /// style 0's value (`scale0`, `d_lightstylevalue[0] / 256`), as the light
    /// is in the style-0 block. A torch at scale 0 adds nothing.
    pub(super) fn add_to(&self, luxels: &mut [f32], scale0: f32) {
        for l in self.lit {
            let k = self.scale(l) * scale0;
            if k == 0.0 {
                continue;
            }
            for (cell, &s) in luxels.iter_mut().zip(l.shares.iter()) {
                *cell += k * s;
            }
        }
    }

    /// The torches' change to luxel `i` of the face, before style 0's value:
    /// what `R_LightPoint` reads changes by, for a model standing over it.
    pub(super) fn at(&self, i: usize) -> f32 {
        self.lit.iter().map(|l| self.scale(l) * l.shares.get(i).copied().unwrap_or(0.0)).sum()
    }

    /// Whether every torch on the face is at scale 0 this frame (the luxels
    /// are id's).
    pub(super) fn is_still(&self) -> bool {
        self.lit.iter().all(|l| self.scale(l) == 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_strength_is_clamped_and_rounded() {
        assert_eq!(TorchFlicker::from_value(0.0), TorchFlicker::OFF);
        assert_eq!(TorchFlicker::from_value(-1.0), TorchFlicker::OFF);
        assert_eq!(TorchFlicker::from_value(f32::NAN), TorchFlicker::OFF);
        assert_eq!(TorchFlicker::from_value(1.0), TorchFlicker::STYLE);
        assert_eq!(TorchFlicker::from_value(0.5).value(), 0.5);
        assert_eq!(TorchFlicker::from_value(9.0).value(), 2.0);
        assert_eq!(TorchFlicker::from_value(f32::INFINITY).value(), 2.0);
        assert_eq!(TorchFlicker::from_value(0.333).value(), 0.33);
    }
}
