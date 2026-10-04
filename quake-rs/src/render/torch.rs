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
//! the light tool computed it, LIGHT.EXE's own sample points and shadow trace
//! ported ([`sample_points`], [`test_line`], [`share`]: `(light - dist)`
//! times the tool's angle and range scales where the line from the torch
//! reaches the point, never more than the luxel holds). Each frame moves that
//! share by the torch's flicker:
//!
//! ```text
//! luxel(t) = id's luxel + strength · depth · Σ_k (s_k(t)/s̄_k - 1)/√2 · share · d_lightstylevalue[0] / 256
//! ```
//!
//! `s_k(t)` are world.qc's two flicker patterns (styles 1 and 6) through the
//! light-style glide ([`crate::server::LerpLightStyles`], stepped as id's when
//! it is off), each at a rate and a depth the kind of flame gives
//! ([`TorchKind::flicker`]) and each torch at its own phases from its origin,
//! so a row of torches never pulses as one; `s̄_k` is each pattern's mean, so
//! the change is zero-mean: over the patterns' periods every luxel's light
//! averages to id's. The picture's mean brightness follows to within about
//! 2%, not exactly: the colormap's rows are not even steps of light (toward
//! its dark end a rise brightens a texel more than a dip darkens it), and a
//! rise past the brightest row is clamped there while the dip is not. It is
//! the light the torch would have given had the mapper set its style to a
//! flicker (what `start`'s torches do), brought back to the steady torch's
//! average — two flickers rather than one so that it wanders like a flame
//! instead of coming round every 1.7 or 2.3 s. `strength` is the cvar
//! ([`TorchFlicker`], 0 to 2).
//!
//! The faces a torch reaches are rebaked when its value moves, as an animated
//! style's are: the lightmap and lit-surface caches key a face on its torches'
//! scales ([`FaceTorches::key_is`]), and the glide moves in whole steps
//! (`server::GLIDE_STEP`), so a block rebakes at most once a step of either
//! voice. A torch's light reaches 300 units, so in a torch-lit room most of
//! the surfaces drawn are rebaked every frame at 72 Hz and about half of them
//! at 480 Hz (`FRAMERATE.md`, "Steady torches that flicker"); the shadows
//! spare the faces a torch cannot see. A torch with an
//! animated style of its own is left to it (no double flicker); one QuakeC
//! switches (style 32 and up) is left alone.
//!
//! Alias models (the monsters, the gun) are lit by the luxel under them
//! (`R_LightPoint`), and pick up that luxel's change ([`FaceTorches::at`]).
//!
//! The time is the scene's, `cl.time` as a `float`: a pure function of it and
//! the torch, so a demo and the live game show the same light at the same
//! time. After about 10 hours in one level a `float` clock holds a value for
//! two 480 Hz frames (and steps 128 times a second after 28); the light-style
//! glide reads the client's `double` instead, but a flame's flicker, whose
//! fastest moves take tens of milliseconds, cannot show it, so it is not
//! worth a second clock in the [`Scene`](super::Scene).

use crate::bsp::{Bsp, CONTENTS_SOLID, DFace, TEX_SPECIAL};
use crate::math::{Vec3, add, cross, dot, mul_add, normalize, scale, sub};
use crate::server::{LerpLightStyles, Tokenizer, lightstyle_value_at};

use super::light::{STYLE_NONE, surface_extents};
use super::surf::face_world_poly;

/// `r_torchflicker`: how much the steady torches flicker, in hundredths of
/// the flicker style's own swing — 0 off (id's: Classic), 100 as if the
/// mapper had given each torch its flicker style, at most 200, which reads
/// as noise more than fire (about three times the movement of id's own
/// flickering torches).
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
        if v > 0.0 { TorchFlicker((v.min(Self::MAX) * 100.0).round() as u8) } else { TorchFlicker::OFF }
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

/// One voice of a flame's flicker: a world.qc flicker pattern, read at `rate`
/// letters a tenth of a second (a light style reads 1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Voice {
    pub(super) pattern: &'static [u8],
    pub(super) rate: f32,
}

/// How a kind of flame flickers: world.qc's two flickers at once, each at its
/// own rate and phase, their swings about their means summed over √2 (so
/// `depth` 1 swings as much as one flicker style does, in the mean square),
/// times `depth`. One pattern alone is a loop — the same rises and the one
/// bright `q` every 1.7 or 2.3 s, a beat the eye finds; two at rates whose
/// periods do not divide wander and never come round the same way, as a
/// flame does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Flicker {
    pub(super) voices: [Voice; 2],
    pub(super) depth: f32,
}

/// The three kinds of flame the shareware and registered maps place
/// (misc.qc's spawn functions; `light_flame_small_white` is the yellow one's
/// white twin).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TorchKind {
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

impl TorchKind {
    fn of_classname(name: &str) -> Option<TorchKind> {
        match name {
            "light_torch_small_walltorch" => Some(TorchKind::WallTorch),
            "light_flame_large_yellow" => Some(TorchKind::LargeFlame),
            "light_flame_small_yellow" | "light_flame_small_white" => Some(TorchKind::SmallFlame),
            _ => None,
        }
    }

    /// The kind's flicker: the wall torch quick and shallow (style 6 at a
    /// light style's own rate, style 1 a little slower), the big flame slow
    /// and deep (both at about half speed), the small flame between.
    pub(super) fn flicker(self) -> Flicker {
        let voice = |pattern, rate| Voice { pattern, rate };
        match self {
            TorchKind::WallTorch => Flicker { voices: [voice(FLICKER_6, 1.0), voice(FLICKER_1, 0.75)], depth: 0.8 },
            TorchKind::SmallFlame => Flicker { voices: [voice(FLICKER_1, 0.8), voice(FLICKER_6, 0.9)], depth: 1.0 },
            TorchKind::LargeFlame => Flicker { voices: [voice(FLICKER_1, 0.5), voice(FLICKER_6, 0.6)], depth: 1.3 },
        }
    }
}

/// `pattern`'s mean light value (`(letter - 'a') * 22` over its letters): the
/// level a flickering light averages over its period, stepped or gliding.
fn pattern_mean(pattern: &[u8]) -> f32 {
    let sum: i32 = pattern.iter().map(|&c| i32::from(c.wrapping_sub(b'a') % 26) * 22).sum();
    sum as f32 / pattern.len().max(1) as f32
}

/// A torch's phase in a pattern of `len` letters, in letters, from its origin
/// (and `voice`, so its two voices start apart): a hash of the whole units
/// the entity lump gives, the same every time the map loads, in the live
/// game and in a demo, neighbouring torches far apart. Sixteenths of a
/// letter, so torches do not even step together.
fn phase_of(origin: Vec3, voice: u32, len: usize) -> f32 {
    let mut h: u32 = 0x811c_9dc5 ^ voice;
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
    pub(super) kind: TorchKind,
    /// Where in each voice's pattern it is at time 0, in letters
    /// ([`phase_of`]).
    phases: [f32; 2],
    /// Each voice's pattern's mean ([`pattern_mean`]).
    means: [f32; 2],
}

impl Torch {
    fn new(origin: Vec3, light: f32, kind: TorchKind) -> Torch {
        let voices = kind.flicker().voices;
        Torch {
            origin,
            light,
            kind,
            phases: [0, 1].map(|k| phase_of(origin, k, voices[k as usize].pattern.len())),
            means: voices.map(|v| pattern_mean(v.pattern)),
        }
    }

    /// The torch's scale this frame: `strength · depth · Σ (s_k(t)/s̄_k - 1) / √2`
    /// over its two voices, the fraction of its share each luxel it lights
    /// gains (or loses) at `time` (`cl.time`). A pure function of the time and
    /// the torch.
    pub(super) fn scale(&self, time: f32, lerp: LerpLightStyles, strength: TorchFlicker) -> f32 {
        if strength.is_off() {
            return 0.0;
        }
        let f = self.kind.flicker();
        let tenths = f64::from(if time.is_finite() { time } else { 0.0 }) * 10.0;
        let mut swing = 0.0f32;
        for (k, v) in f.voices.iter().enumerate() {
            let value = lightstyle_value_at(v.pattern, tenths * f64::from(v.rate) + f64::from(self.phases[k]), lerp);
            swing += value as f32 / self.means[k] - 1.0;
        }
        strength.value() * f.depth * swing * std::f32::consts::FRAC_1_SQRT_2
    }
}

/// LIGHT.EXE's `DEFAULTLIGHTLEVEL` (`entities.h`): a `light*` entity without
/// a `light` key (or with 0) shines at 300, the torches too (misc.qc's
/// "Default light value is 200" is the editor's note, not the tool's).
const DEFAULT_LIGHT: f32 = 300.0;

/// An entity as LIGHT.EXE's `LoadEntities` (`entities.c`) reads it, for the
/// fields its lighting uses: the class, the origin, the light — any key
/// starting `light`, or `_light`, the last one given; `DEFAULTLIGHTLEVEL` for
/// a `light*` class without one — and the style (`atof` into an `int`). The
/// tool lights every entity whose light is not 0 (`LightFace`).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct LightEntity {
    pub(super) class: String,
    pub(super) origin: Vec3,
    pub(super) light: f32,
    pub(super) style: i32,
}

/// The entities of an entity lump with what LIGHT.EXE reads of them
/// ([`LightEntity`]); an entity without an origin is at the origin, as the
/// tool's zeroed entity is.
pub(super) fn light_entities(entities: &str) -> Vec<LightEntity> {
    let mut out = Vec::new();
    let mut tok = Tokenizer::new(entities);
    while let Some(open) = tok.next_token() {
        if open != "{" {
            break;
        }
        let mut e = LightEntity { class: String::new(), origin: [0.0; 3], light: 0.0, style: 0 };
        while let Some(key) = tok.next_token() {
            if key == "}" {
                break;
            }
            let Some(value) = tok.next_token() else { break };
            match key.as_str() {
                "classname" => e.class = value,
                "origin" => e.origin = crate::server::parse_vector(&value),
                k if k.starts_with("light") || k == "_light" => e.light = crate::server::parse_float(&value),
                "style" => e.style = crate::server::parse_float(&value) as i32,
                _ => {}
            }
        }
        if e.class.starts_with("light") && e.light == 0.0 {
            e.light = DEFAULT_LIGHT;
        }
        out.push(e);
    }
    out
}

/// The steady torches in an entity lump: every flame class with no `style`
/// (or style 0). A torch with an animated style is the light style's to
/// animate; one with a switched style (32 and up) is QuakeC's. (A torch with
/// a `target` would be a spotlight to the tool; no torch of id's maps has
/// one, and this lights it as a point.)
pub(super) fn steady_torches(entities: &str) -> Vec<Torch> {
    light_entities(entities)
        .into_iter()
        .filter(|e| e.style == 0)
        .filter_map(|e| Some(Torch::new(e.origin, e.light, TorchKind::of_classname(&e.class)?)))
        .collect()
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
/// `(1 - scalecos) + scalecos * cos(angle)`, nothing past `light` — where the
/// tool's `CastRay` reached the point ([`test_line`]; [`TorchSet::build`]
/// asks it).
pub(super) fn share(origin: Vec3, light: f32, p: Vec3, normal: Vec3) -> f32 {
    let d = [origin[0] - p[0], origin[1] - p[1], origin[2] - p[2]];
    let dist = dot(d, d).sqrt();
    if dist.is_nan() || dist >= light {
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
    /// Per world face (`bsp.faces`), the torches that light it.
    faces: Vec<Box<[TorchLit]>>,
    /// Each torch's scale this frame ([`Torch::scale`]).
    scales: Vec<f32>,
}

impl TorchSet {
    /// The steady torches of `world`'s entity lump and, for every face of the
    /// world and its brush models one reaches, its shares of the face's
    /// luxels, as the light tool lit it: the faces it is in front of
    /// (`SingleLightFace`: `dist <= 0` or `dist > light` skip the face), at
    /// the tool's sample points ([`sample_points`]), each where the line from
    /// the torch reaches it through the world's nodes ([`test_line`]) and
    /// nothing where a wall is in the way. The shares of a luxel are bounded
    /// by the luxel its style-0 block holds — where the tool clamped the sum
    /// at 255, a torch gives no more than is there — and a face without a
    /// style-0 block gets nothing.
    ///
    /// The tool's traces are most of the work (tens to hundreds of
    /// milliseconds a map on one thread), so the faces are shared out over
    /// `threads` threads, the calling thread one of them; each face's shares
    /// are its own, so the set is the same for any count.
    pub(super) fn build(world: &Bsp, threads: usize) -> TorchSet {
        let torches = steady_torches(&world.entities);
        let mut faces: Vec<Box<[TorchLit]>> = Vec::new();
        faces.resize_with(world.faces.len(), Default::default);
        if !torches.is_empty() && !world.lighting.is_empty() {
            super::band::for_rows(threads, faces.len(), &mut faces, 1, |first, run| {
                let mut poly = Vec::new();
                for (k, lit) in run.iter_mut().enumerate() {
                    *lit = face_shares(world, &world.faces[first + k], &torches, &mut poly).into_boxed_slice();
                }
            });
        }
        let scales = vec![0.0; torches.len()];
        TorchSet { torches, faces, scales }
    }

    /// Set every torch's scale for the frame at `time` (`cl.time`).
    pub(super) fn animate(&mut self, time: f32, lerp: LerpLightStyles, strength: TorchFlicker) {
        for (s, t) in self.scales.iter_mut().zip(&self.torches) {
            *s = t.scale(time, lerp, strength);
        }
    }

    /// The torches lighting world face `fi`, at this frame's scales.
    pub(super) fn face(&self, fi: usize) -> FaceTorches<'_> {
        let lit = self.faces.get(fi).map_or(&[][..], |l| &l[..]);
        FaceTorches { lit, scales: &self.scales }
    }

    /// The steady torches found, and the (face, torch) pairs that light.
    #[cfg(test)]
    pub(super) fn counts(&self) -> (usize, usize) {
        (self.torches.len(), self.faces.iter().map(|l| l.len()).sum())
    }

    #[cfg(test)]
    pub(super) fn torches(&self) -> &[Torch] {
        &self.torches
    }
}

/// [`TorchSet::build`] for one face: a [`TorchLit`] for each torch that
/// lights it, its shares bounded by what each luxel holds.
fn face_shares(bsp: &Bsp, face: &DFace, torches: &[Torch], poly: &mut Vec<Vec3>) -> Vec<TorchLit> {
    let Some(FaceShares { block, grids }) = unbounded_shares(bsp, face, torches, poly) else { return Vec::new() };
    let mut lit: Vec<TorchLit> = grids
        .into_iter()
        .filter(|(_, shares)| shares.iter().any(|&s| s >= MIN_SHARE))
        .map(|(torch, shares)| TorchLit { torch, shares: shares.into_boxed_slice() })
        .collect();
    for (j, &held) in block.iter().enumerate() {
        let total: f32 = lit.iter().map(|l| l.shares[j]).sum();
        let held = f32::from(held);
        if total > held {
            let k = if total > 0.0 { held / total } else { 0.0 };
            for l in lit.iter_mut() {
                l.shares[j] *= k;
            }
        }
    }
    lit
}

/// A face's style-0 block and, for each torch in front of it and in reach,
/// its [`share`] of each luxel — before the bound by the block.
struct FaceShares<'a> {
    block: &'a [u8],
    grids: Vec<(u32, Vec<f32>)>,
}

/// [`FaceShares`] for `face`, or `None` when no torch reaches it or it has no
/// style-0 block (a sky or liquid, a face without samples, one only a
/// switched or animated light reached).
fn unbounded_shares<'a>(bsp: &'a Bsp, face: &DFace, torches: &[Torch], poly: &mut Vec<Vec3>) -> Option<FaceShares<'a>> {
    let ti = usize::try_from(face.texinfo).ok().and_then(|i| bsp.texinfo.get(i))?;
    if ti.flags & TEX_SPECIAL != 0 || face.lightofs < 0 {
        return None;
    }
    // The torch's light is in the face's style-0 block.
    let slot = face.styles.iter().take_while(|&&s| s != STYLE_NONE).position(|&s| s == 0)?;
    let plane = usize::try_from(face.planenum).ok().and_then(|i| bsp.planes.get(i))?;
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
        return None;
    }
    let (texmins, extent) = surface_extents(ti, poly)?;
    let (lmw, lmh) = ((extent[0] / 16 + 1) as usize, (extent[1] / 16 + 1) as usize);
    let n = lmw * lmh;
    let block = usize::try_from(face.lightofs)
        .ok()
        .and_then(|o| o.checked_add(slot * n))
        .and_then(|o| bsp.lighting.get(o..o.checked_add(n)?))?;
    let points = sample_points(bsp, ti, (normal, facedist), poly, texmins, (lmw, lmh))?;
    let mut sub = vec![0.0f32; points.len()];
    let grids = reaching
        .iter()
        .map(|&i| {
            let t = &torches[i as usize];
            for (cell, &p) in sub.iter_mut().zip(&points) {
                let s = share(t.origin, t.light, p, normal);
                // `CastRay(light->origin, surf)`: blocked, nothing.
                *cell = if s > 0.0 && test_line(bsp, t.origin, p) { s } else { 0.0 };
            }
            // `LightFace`'s "extra filtering": a luxel is its four samples'
            // mean.
            let w = lmw * 2;
            let luxel = |j: usize| {
                let (s, t) = (j % lmw, j / lmw);
                let at = |k: usize| sub[k];
                (at(t * 2 * w + s * 2)
                    + at(t * 2 * w + s * 2 + 1)
                    + at((t * 2 + 1) * w + s * 2)
                    + at((t * 2 + 1) * w + s * 2 + 1))
                    * 0.25
            };
            (i, (0..n).map(luxel).collect())
        })
        .collect();
    Some(FaceShares { block, grids })
}

// ---------------------------------------------------------------------------
// LIGHT.EXE's sample points and shadows (LTFACE.C, TRACE.C)
// ---------------------------------------------------------------------------

/// `ON_EPSILON` (`light.h`): a segment within this of a plane at both ends is
/// on that side of it whole, not split.
const ON_EPSILON: f32 = 0.1;

/// `TestLine` (`trace.c`): whether the segment from `start` to `stop` crosses
/// no solid leaf of the world's node tree (`MakeTnodes(&dmodels[0])`: brush
/// models are traced against the world alone, and only `CONTENTS_SOLID`
/// blocks — water and sky let light through). id's walks the tree with a
/// stack of back halves; this recursion is the same walk: the near side of a
/// split first, then the far side from the split point. A world without
/// nodes (the synthetic test rooms) blocks nothing.
///
/// The game's own trace (`world::trace_world`, `SV_RecursiveHullCheck`) is
/// not the tool's: it splits at every plane the segment crosses, where the
/// tool keeps a segment within [`ON_EPSILON`] of a plane at both ends on one
/// side, and it rebuilds the hull and finds the impact, which a yes or no
/// does not need.
pub(super) fn test_line(bsp: &Bsp, start: Vec3, stop: Vec3) -> bool {
    let head = bsp.models.first().map_or(0, |m| m.headnode[0]);
    bsp.nodes.is_empty() || line_clear(bsp, head, start, stop, bsp.nodes.len())
}

/// One node of [`test_line`]'s walk; `depth` bounds it (a malformed tree).
fn line_clear(bsp: &Bsp, node: i32, front: Vec3, back: Vec3, depth: usize) -> bool {
    if node < 0 {
        // A leaf: `-(leaf + 1)`.
        let leaf = usize::try_from(-(node + 1)).ok().and_then(|l| bsp.leafs.get(l));
        return leaf.map_or(true, |l| l.contents != CONTENTS_SOLID);
    }
    let Some(n) = usize::try_from(node).ok().and_then(|i| bsp.nodes.get(i)) else { return true };
    let Some(plane) = usize::try_from(n.planenum).ok().and_then(|i| bsp.planes.get(i)) else { return true };
    if depth == 0 {
        return true;
    }
    let child = |k: usize| i32::from(n.children[k]);
    // The axial planes by their coordinate, as `MakeTnode`'s `type`.
    let (f, b) = match plane.ptype {
        t @ 0..=2 => (front[t as usize] - plane.dist, back[t as usize] - plane.dist),
        _ => (dot(front, plane.normal) - plane.dist, dot(back, plane.normal) - plane.dist),
    };
    if f > -ON_EPSILON && b > -ON_EPSILON {
        return line_clear(bsp, child(0), front, back, depth - 1);
    }
    if f < ON_EPSILON && b < ON_EPSILON {
        return line_clear(bsp, child(1), front, back, depth - 1);
    }
    let side = usize::from(f < 0.0);
    let frac = f / (f - b);
    let mid = [0, 1, 2].map(|j| front[j] + frac * (back[j] - front[j]));
    line_clear(bsp, child(side), front, mid, depth - 1) && line_clear(bsp, child(1 - side), mid, back, depth - 1)
}

/// The world points LIGHT.EXE lit a face's luxels at: `CalcFaceVectors`,
/// `CalcFaceExtents` and `CalcPoints` (`ltface.c`) with `-extra`, which id
/// lit its maps with (the shareware episode's style-0 lightmaps are this
/// re-derivation's to the byte on 85–100% of their luxels, `start` on all;
/// without `-extra`, on a quarter): four samples a luxel, `2 lmw x 2 lmh`
/// row-major, sample `(s, t)` at texture point `texturemins - 8 + 8 * (s,
/// t)`, so luxel `(s, t)` is the mean of its samples `(2s, 2t)` to `(2s + 1,
/// 2t + 1)`. Each projected back along the texture axes' normal onto the
/// face's plane a unit in front of it (`plane`, turned to the face's side) —
/// and, where
/// the face's middle cannot see that point (a grid point past the face's edge,
/// inside a wall), moved 8 texels toward the middle in t, then s, up to six
/// times, the last time also 8 units toward the middle ("this doesn't
/// completely work", the tool's comment says: light bleeds under walls a
/// little, and the bake has it). `None` for a texture axis along the plane
/// (the tool's "Texture axis perpendicular to face").
fn sample_points(
    bsp: &Bsp,
    ti: &crate::bsp::TexInfo,
    (normal, facedist): (Vec3, f32),
    poly: &[Vec3],
    texmins: [i32; 2],
    (lmw, lmh): (usize, usize),
) -> Option<Vec<Vec3>> {
    let axis = |k: usize| [ti.vecs[k][0], ti.vecs[k][1], ti.vecs[k][2]];
    let (s_axis, t_axis) = (axis(0), axis(1));
    // CalcFaceVectors: the normal to the texture axes, turned to the plane's
    // side; `distscale`, the distance along it per unit along the normal.
    let (mut texnormal, len) = normalize(cross(t_axis, s_axis));
    let mut distscale = dot(texnormal, normal);
    if len == 0.0 || distscale == 0.0 || distscale.is_nan() {
        return None;
    }
    if distscale < 0.0 {
        distscale = -distscale;
        texnormal = scale(texnormal, -1.0);
    }
    let distscale = 1.0 / distscale;
    let textoworld = [s_axis, t_axis].map(|v| {
        let l = dot(v, v).sqrt();
        scale(mul_add(v, -dot(v, normal) * distscale, texnormal), (1.0 / l) * (1.0 / l))
    });
    let texorg = add(scale(textoworld[0], -ti.vecs[0][3]), scale(textoworld[1], -ti.vecs[1][3]));
    let d = (dot(texorg, normal) - facedist - 1.0) * distscale;
    let texorg = mul_add(texorg, -d, texnormal);
    let at = |us: f32, ut: f32| add(texorg, add(scale(textoworld[0], us), scale(textoworld[1], ut)));
    // CalcFaceExtents' exact extents, and the face's middle.
    let (mut mins, mut maxs) = ([f32::MAX; 2], [f32::MIN; 2]);
    for p in poly {
        for (j, v) in [s_axis, t_axis].iter().enumerate() {
            let val = dot(*p, *v) + ti.vecs[j][3];
            (mins[j], maxs[j]) = (mins[j].min(val), maxs[j].max(val));
        }
    }
    let (mids, midt) = ((maxs[0] + mins[0]) / 2.0, (maxs[1] + mins[1]) / 2.0);
    let facemid = at(mids, midt);
    // A step of 8 toward the middle, never past it.
    let toward = |u: f32, mid: f32| if u > mid { (u - 8.0).max(mid) } else { (u + 8.0).min(mid) };
    let (w, h) = (lmw * 2, lmh * 2);
    let mut points = Vec::with_capacity(w * h);
    for t in 0..h {
        for s in 0..w {
            let mut us = (texmins[0] - 8 + 8 * s as i32) as f32;
            let mut ut = (texmins[1] - 8 + 8 * t as i32) as f32;
            let mut surf = at(us, ut);
            for i in 0..6 {
                if test_line(bsp, facemid, surf) {
                    break;
                }
                if i & 1 != 0 {
                    us = toward(us, mids);
                } else {
                    ut = toward(ut, midt);
                }
                // The next try; after the last, this one moved 8 units on.
                surf = if i < 5 { at(us, ut) } else { mul_add(surf, 8.0, normalize(sub(facemid, surf)).0) };
            }
            points.push(surf);
        }
    }
    Some(points)
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
    use crate::render::light::{LightMap, face_lightmap_with};
    use crate::render::{Camera, RenderOptions, Renderer, Scene, VideoCvars, demo_room};
    use LerpLightStyles::{Classic, Smooth};

    const KINDS: [TorchKind; 3] = [TorchKind::WallTorch, TorchKind::SmallFlame, TorchKind::LargeFlame];

    /// `demo_room` lit as one style-0 block of `luxel` everywhere, with
    /// `entities` for its entity lump. Its floor (face 0, z = -128, s = x,
    /// t = y) is the face the torches light; its walls' texture axes lie
    /// along their normals, which the tool cannot light.
    fn torch_room(entities: &str, luxel: u8) -> Bsp {
        let mut bsp = demo_room();
        bsp.lighting = vec![luxel; 200_000];
        for f in bsp.faces.iter_mut() {
            f.lightofs = 0;
            f.styles = [0, STYLE_NONE, STYLE_NONE, STYLE_NONE];
        }
        bsp.entities = entities.into();
        bsp
    }

    fn lump(torches: &[&str]) -> String {
        let mut e = String::from("{ \"classname\" \"worldspawn\" \"wad\" \"gfx/base.wad\" }\n");
        for t in torches {
            e += &format!("{{ {t} }}\n");
        }
        e
    }

    /// A big flame 128 units above the floor's middle.
    const FLAME: &str = "\"classname\" \"light_flame_large_yellow\" \"origin\" \"0 128 0\" \"light\" \"300\"";

    fn torch(kind: TorchKind, origin: Vec3) -> Torch {
        Torch::new(origin, 300.0, kind)
    }

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
        assert_eq!(VideoCvars::CLASSIC.torches, TorchFlicker::OFF, "off in Classic");
        assert!(!VideoCvars::MODERN.torches.is_off(), "on in 2026");
    }

    /// The steady torches are the four flame classes with style 0 (or none):
    /// an animated style (start's 1 and 6) is the light style's, a switched
    /// one (32 and up) QuakeC's. The light is ENTITIES.C's: any key starting
    /// `light`, or `_light`, the last one given; none, or 0, is 300. No
    /// origin is the tool's zeroed origin.
    #[test]
    fn the_steady_torches_are_the_flames_without_a_style() {
        let e = lump(&[
            "\"classname\" \"light_torch_small_walltorch\" \"origin\" \"1 2 3\" \"light\" \"200\"",
            "\"classname\" \"light_flame_large_yellow\" \"origin\" \"4 5 6\"",
            "\"origin\" \"7 8 9\" \"light\" \"0\" \"classname\" \"light_flame_small_yellow\" \"style\" \"0\"",
            "\"classname\" \"light_flame_small_white\" \"origin\" \"-10 -11 -12\" \"light\" \"250\"",
            "\"classname\" \"light_torch_small_walltorch\" \"origin\" \"0 0 0\" \"style\" \"1\"",
            "\"classname\" \"light_torch_small_walltorch\" \"origin\" \"0 0 0\" \"style\" \"6\"",
            "\"classname\" \"light_flame_large_yellow\" \"origin\" \"0 0 0\" \"style\" \"11\"",
            "\"classname\" \"light_flame_small_yellow\" \"origin\" \"0 0 0\" \"style\" \"32\"",
            "\"classname\" \"light\" \"origin\" \"0 0 0\"",
            "\"classname\" \"light_fluoro\" \"origin\" \"0 0 0\"",
            "\"classname\" \"light_torch_small_walltorch\"",
            "\"classname\" \"light_flame_small_yellow\" \"origin\" \"1 1 1\" \"_light\" \"150\"",
            "\"classname\" \"light_flame_small_yellow\" \"origin\" \"2 2 2\" \"light\" \"150\" \"lightlevel\" \"175\"",
            "\"classname\" \"light_flame_small_yellow\" \"origin\" \"3 3 3\" \"style\" \"0.7\"",
        ]);
        let got: Vec<(Vec3, f32, TorchKind)> = steady_torches(&e).iter().map(|t| (t.origin, t.light, t.kind)).collect();
        assert_eq!(
            got,
            [
                ([1.0, 2.0, 3.0], 200.0, TorchKind::WallTorch),
                ([4.0, 5.0, 6.0], 300.0, TorchKind::LargeFlame),
                ([7.0, 8.0, 9.0], 300.0, TorchKind::SmallFlame),
                ([-10.0, -11.0, -12.0], 250.0, TorchKind::SmallFlame),
                ([0.0, 0.0, 0.0], 300.0, TorchKind::WallTorch),
                ([1.0, 1.0, 1.0], 150.0, TorchKind::SmallFlame),
                ([2.0, 2.0, 2.0], 175.0, TorchKind::SmallFlame),
                ([3.0, 3.0, 3.0], 300.0, TorchKind::SmallFlame),
            ]
        );
        // The tool lights every entity with a light: a plain `light` too.
        let all = light_entities(&e);
        assert_eq!(
            all.iter().filter(|l| l.class == "light" || l.class == "light_fluoro").map(|l| l.light).collect::<Vec<_>>(),
            [300.0, 300.0]
        );
        assert_eq!(all[0].light, 0.0, "worldspawn has no light");
        assert!(steady_torches("").is_empty() && steady_torches("garbage { \"").is_empty());
    }

    /// `SingleLightFace`'s add times `rangescale`: `(light - dist)` times
    /// `0.5 + 0.5 cos`, halved; nothing at or past `light`, half the light
    /// edge-on, none from straight behind.
    #[test]
    fn the_share_falls_off_as_the_light_tool_lit() {
        let up = [0.0, 0.0, 1.0];
        assert_eq!(share([0.0, 0.0, 100.0], 300.0, [0.0; 3], up), 100.0, "(300 - 100) * 1 * 0.5");
        assert_eq!(share([0.0, 0.0, 250.0], 300.0, [0.0; 3], up), 25.0);
        assert_eq!(share([0.0, 0.0, 300.0], 300.0, [0.0; 3], up), 0.0, "at the light's reach");
        assert_eq!(share([0.0, 0.0, 400.0], 300.0, [0.0; 3], up), 0.0, "past it");
        assert_eq!(share([100.0, 0.0, 0.0], 300.0, [0.0; 3], up), 50.0, "edge-on: half");
        assert_eq!(share([0.0, 0.0, -100.0], 300.0, [0.0; 3], up), 0.0, "from behind");
        let slant = share([60.0, 0.0, 80.0], 300.0, [0.0; 3], up); // dist 100, cos 0.8
        assert!((slant - 200.0 * 0.9 * 0.5).abs() < 1e-3, "{slant}");
        // Linear in the distance.
        let s: Vec<f32> =
            [50.0, 100.0, 150.0, 200.0].iter().map(|&z| share([0.0, 0.0, z], 300.0, [0.0; 3], up)).collect();
        assert_eq!(s, [125.0, 100.0, 75.0, 50.0]);
    }

    /// A torch's phases are a function of its origin alone — the same each
    /// time — and a row of torches a few units apart start nowhere near
    /// together.
    #[test]
    fn the_phase_is_the_origins() {
        let row: Vec<Torch> = (0..8).map(|k| torch(TorchKind::WallTorch, [64.0 * k as f32, 512.0, 96.0])).collect();
        let again: Vec<Torch> = (0..8).map(|k| torch(TorchKind::WallTorch, [64.0 * k as f32, 512.0, 96.0])).collect();
        assert_eq!(row, again);
        for t in &row {
            for (k, v) in TorchKind::WallTorch.flicker().voices.iter().enumerate() {
                assert!((0.0..v.pattern.len() as f32).contains(&t.phases[k]));
            }
        }
        let mut phases: Vec<u32> = row.iter().map(|t| (t.phases[0] * 16.0) as u32).collect();
        phases.sort_unstable();
        phases.dedup();
        assert_eq!(phases.len(), row.len(), "no two of the row in step");
        // Different scales at one instant: they do not pulse as one.
        let at = |t: &Torch| t.scale(12.34, Smooth, TorchFlicker::STYLE);
        let distinct: std::collections::BTreeSet<u32> = row.iter().map(|t| at(t).to_bits()).collect();
        assert!(distinct.len() >= 6, "{distinct:?}");
        // A whole-unit origin, however it is written, is the same torch.
        assert_eq!(phase_of([1.0, 2.0, 3.0], 0, 23), phase_of([1.2, 1.9, 3.4], 0, 23));
    }

    /// The flicker is zero-mean: over a long run of frames each kind's scale
    /// averages to 0, stepped or gliding — so the light averages to id's —
    /// and it moves, more for the big flame than the wall torch.
    #[test]
    fn the_flicker_is_zero_mean() {
        for flame in KINDS {
            let t = torch(flame, [100.0, -200.0, 64.0]);
            for lerp in [Classic, Smooth] {
                // 400 s at 120 Hz: past each kind's two periods' common one.
                let n = 48_000;
                let (mut sum, mut lo, mut hi) = (0.0f64, f32::MAX, f32::MIN);
                for f in 0..n {
                    let s = t.scale(f as f32 / 120.0, lerp, TorchFlicker::STYLE);
                    sum += f64::from(s);
                    (lo, hi) = (lo.min(s), hi.max(s));
                }
                let mean = sum / n as f64;
                assert!(mean.abs() < 2e-3, "{flame:?} {lerp:?}: mean {mean}");
                assert!(lo < -0.05 && hi > 0.15, "{flame:?} {lerp:?}: {lo}..{hi}");
            }
        }
        let swing = |flame| {
            let t = torch(flame, [0.0; 3]);
            (0..24_000).map(|f| t.scale(f as f32 / 120.0, Smooth, TorchFlicker::STYLE).powi(2)).sum::<f32>()
        };
        assert!(
            swing(TorchKind::LargeFlame) > swing(TorchKind::SmallFlame)
                && swing(TorchKind::SmallFlame) > swing(TorchKind::WallTorch)
        );
    }

    /// The strength scales the swing exactly and 0 is still; the scale is a
    /// pure function of the time and the torch (any clock is safe).
    #[test]
    fn the_strength_scales_the_swing() {
        let t = torch(TorchKind::SmallFlame, [3.0, 4.0, 5.0]);
        for f in 0..2000 {
            let time = f as f32 * 0.0131;
            let full = t.scale(time, Smooth, TorchFlicker::STYLE);
            assert_eq!(t.scale(time, Smooth, TorchFlicker::from_value(0.5)), full * 0.5);
            assert_eq!(t.scale(time, Smooth, TorchFlicker::from_value(2.0)), full * 2.0);
            assert_eq!(t.scale(time, Smooth, TorchFlicker::OFF), 0.0);
            assert_eq!(t.scale(time, Smooth, TorchFlicker::STYLE).to_bits(), full.to_bits());
        }
        for time in [f32::NAN, f32::INFINITY, -1e30, 1e30, -5.0] {
            assert!(t.scale(time, Smooth, TorchFlicker::from_value(2.0)).abs() < 2.0, "{time}");
        }
    }

    /// The faces a torch lights: those it is in front of and within its
    /// light of, at the tool's sample points; every share bounded by its
    /// luxel; nothing for a face with no style-0 block or no samples.
    #[test]
    fn the_shares_are_the_tools_and_bounded_by_the_luxel() {
        let bsp = torch_room(&lump(&[FLAME]), 255);
        let set = TorchSet::build(&bsp, 1);
        assert_eq!(set.counts().0, 1);
        let floor = set.face(0);
        assert!(!floor.is_empty(), "the floor is lit");
        assert!(
            set.face(1).is_empty() || set.face(1).lit.iter().all(|l| l.torch == 0),
            "the ceiling, if lit, by the flame"
        );
        // The floor's luxel under the flame (x = 0, y = 128 -> luxel (16, 24)
        // of 33 from texmins -256) is the mean of its four samples, 8 units
        // apart below and left of it, a unit above the floor.
        let lit = &floor.lit[0];
        let under = 24 * 33 + 16;
        let at = |x: f32, y: f32| share([0.0, 128.0, 0.0], 300.0, [x, y, -127.0], [0.0, 0.0, 1.0]);
        let want = (at(-8.0, 120.0) + at(0.0, 120.0) + at(-8.0, 128.0) + at(0.0, 128.0)) * 0.25;
        assert!((lit.shares[under] - want).abs() < 1e-3, "{} vs {want}", lit.shares[under]);
        assert!((at(0.0, 128.0) - (300.0 - 127.0) * 0.5).abs() < 1e-3);
        // A far corner of the floor is out of reach.
        assert_eq!(lit.shares[0], 0.0);

        // A dim room: no share is more than the luxel holds, two torches
        // together neither.
        let two = lump(&[FLAME, "\"classname\" \"light_flame_large_yellow\" \"origin\" \"0 100 -100\""]);
        let dim = TorchSet::build(&torch_room(&two, 10), 1);
        let lits = dim.face(0).lit;
        assert_eq!(lits.len(), 2);
        for j in 0..lits[0].shares.len() {
            let sum: f32 = lits.iter().map(|l| l.shares[j]).sum();
            assert!(sum <= 10.0 + 1e-4, "luxel {j}: {sum}");
        }
        assert!(lits.iter().any(|l| l.shares.iter().any(|&s| s > 9.0)), "the bound, not nothing");

        // Behind the floor, or past its reach above it: nothing.
        for far in ["0 0 -200", "0 0 400"] {
            let e = lump(&[&format!("\"classname\" \"light_flame_large_yellow\" \"origin\" \"{far}\"")]);
            assert!(TorchSet::build(&torch_room(&e, 200), 1).face(0).is_empty(), "{far}");
        }
        // No style-0 block (lit only by a switched light), or no samples.
        let mut bsp = torch_room(&lump(&[FLAME]), 200);
        bsp.faces[0].styles = [32, STYLE_NONE, STYLE_NONE, STYLE_NONE];
        bsp.faces[1].lightofs = -1;
        let set = TorchSet::build(&bsp, 1);
        assert!(set.face(0).is_empty() && set.face(1).is_empty());
        // The style-0 block where it is the second.
        bsp.faces[0].styles = [32, 0, STYLE_NONE, STYLE_NONE];
        assert!(!TorchSet::build(&bsp, 1).face(0).is_empty());
    }

    /// `torch_room` with a wall across it: the world's node tree cuts the
    /// slab `x0 <= x <= x1` out solid, the rest empty (two planes, a solid
    /// and an empty leaf).
    fn walled_room(entities: &str, luxel: u8, x0: f32, x1: f32) -> Bsp {
        use crate::bsp::{CONTENTS_EMPTY, DLeaf, DNode, DPlane};
        let mut bsp = torch_room(entities, luxel);
        let p = bsp.planes.len() as i32;
        for dist in [x0, x1] {
            bsp.planes.push(DPlane { normal: [1.0, 0.0, 0.0], dist, ptype: 0 });
        }
        let leaf = |contents| DLeaf {
            contents,
            visofs: -1,
            mins: [0; 3],
            maxs: [0; 3],
            firstmarksurface: 0,
            nummarksurfaces: 0,
            ambient_level: [0; 4],
        };
        bsp.leafs = vec![leaf(CONTENTS_SOLID), leaf(CONTENTS_EMPTY)];
        // Children: a node's number, or -(leaf + 1): -1 solid, -2 empty.
        let node =
            |planenum, children| DNode { planenum, children, mins: [0; 3], maxs: [0; 3], firstface: 0, numfaces: 0 };
        bsp.nodes = vec![node(p, [1, -2]), node(p + 1, [-2, -1])];
        bsp
    }

    /// `TestLine`: a segment through the slab is blocked, one on either side
    /// of it is not, nor one that only touches a plane within `ON_EPSILON`;
    /// a world without nodes blocks nothing.
    #[test]
    fn test_line_is_the_tools() {
        let bsp = walled_room(&lump(&[]), 100, 60.0, 70.0);
        assert!(test_line(&bsp, [0.0, 0.0, 0.0], [50.0, 90.0, -100.0]));
        assert!(test_line(&bsp, [80.0, 0.0, 0.0], [200.0, -90.0, 100.0]));
        assert!(!test_line(&bsp, [0.0, 0.0, 0.0], [100.0, 0.0, 0.0]));
        assert!(!test_line(&bsp, [100.0, 50.0, 3.0], [0.0, 0.0, 0.0]), "either way");
        assert!(!test_line(&bsp, [65.0, 0.0, 0.0], [65.0, 10.0, 0.0]), "inside the wall");
        assert!(test_line(&bsp, [0.0, 0.0, 0.0], [60.05, 0.0, 0.0]), "within 0.1 of the plane: still in front");
        assert!(!test_line(&bsp, [0.0, 0.0, 0.0], [60.2, 0.0, 0.0]));
        assert!(test_line(&torch_room(&lump(&[]), 100), [0.0; 3], [1000.0, 0.0, 0.0]));
    }

    /// The shadows: a torch behind a wall gives the floor past it nothing,
    /// and where the floor sees it the shares are the open room's, bit for
    /// bit — the trace takes light away, it never moves it. (The luxels by
    /// the wall's foot keep a little: the tool pulled their samples toward
    /// the face's middle, out from under the wall, and baked them so.)
    #[test]
    fn a_torch_behind_a_wall_lights_nothing_past_it() {
        let e = lump(&["\"classname\" \"light_flame_large_yellow\" \"origin\" \"0 0 0\""]);
        let open = TorchSet::build(&torch_room(&e, 255), 1);
        let walled = TorchSet::build(&walled_room(&e, 255, 60.0, 70.0), 1);
        // The floor: 33 x 33 luxels from texmins -256, luxel s at x = 16 s - 256.
        let (open, walled) = (&open.face(0).lit[0].shares, &walled.face(0).lit[0].shares);
        let x = |j: usize| (j % 33) as f32 * 16.0 - 256.0;
        let (mut kept, mut gone) = (0, 0);
        for j in 0..open.len() {
            if x(j) <= 48.0 {
                assert_eq!(walled[j].to_bits(), open[j].to_bits(), "luxel {j} at x {}: in sight, as before", x(j));
                kept += usize::from(open[j] > 0.0);
            } else if x(j) >= 112.0 {
                assert_eq!(walled[j], 0.0, "luxel {j} at x {}: behind the wall", x(j));
                gone += usize::from(open[j] > 0.0);
            }
        }
        assert!(kept > 300 && gone > 100, "{kept} kept, {gone} taken away");
        // A face wholly behind the wall: none of it.
        let far = lump(&["\"classname\" \"light_flame_large_yellow\" \"origin\" \"-150 0 120\""]);
        let behind = walled_room(&far, 255, -100.0, -90.0);
        let set = TorchSet::build(&behind, 1);
        assert!(set.face(10).is_empty(), "the pillar's top, past the wall");
        assert!(!TorchSet::build(&torch_room(&far, 255), 1).face(10).is_empty(), "lit without it");
    }

    /// The review's worst view, e1m2 from (864, -312, 464) looking north: a
    /// dark room's wall with a wall torch behind it, which flickered all
    /// over (94% of the view at 640x400) — and e1m2's arch at the start,
    /// lit by its two flames in sight. In each, every pixel that differs from
    /// id's frame at some time must show a surface a torch in reach can see:
    /// the pixel's ray traced to the first solid, two units back toward the
    /// eye, and a clear `TestLine` from the torch (when id's pak is here) —
    /// but along a shadow's edge, where a luxel 16 units off is lit and the
    /// lightmap's interpolation carries some of it over (as the bake does):
    /// 3.5% of the arch's flickering pixels, a line along each shadow's edge.
    #[test]
    fn no_light_through_walls_in_the_reviews_worst_view() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../quake-data/ID1/PAK0.PAK");
        let Ok(pak) = crate::pak::Pak::open(&path) else {
            eprintln!("skipped: no shareware pak at {}", path.display());
            return;
        };
        let read = |n: &str| pak.read_file(n).expect("read").expect(n);
        let bsp = Bsp::parse(&read("maps/e1m2.bsp")).expect("e1m2");
        let palette = crate::render::parse_palette(&read("gfx/palette.lmp")).expect("palette");
        let colormap = read("gfx/colormap.lmp");
        let mut styles = crate::render::NEUTRAL_LIGHTSTYLE_SCALES;
        styles[0] = 264.0 / 256.0;
        let torches = steady_torches(&bsp.entities);
        let (w, h) = (320usize, 200usize);
        // (pixels that flicker, of them those whose surface no torch sees)
        let audit = |eye: Vec3, yaw: f32| {
            let cam = Camera { pos: eye, yaw, pitch: 0.0, roll: 0.0, fov_deg: 90.0 };
            let frame = |r: &mut Renderer, time: f32, torches| {
                let options =
                    RenderOptions { video: VideoCvars { torches, ..VideoCvars::CLASSIC }, ..RenderOptions::default() };
                let scene = Scene {
                    time,
                    colormap: Some(&colormap),
                    light_styles: &styles,
                    options,
                    ..Scene::new(&bsp, cam, w, h, &palette)
                };
                r.render(&scene).pixels
            };
            let (mut lit, mut id) = (Renderer::new(), Renderer::new());
            let base = frame(&mut id, 0.0, TorchFlicker::OFF);
            let mut moved = vec![false; w * h];
            for k in 0..24 {
                let f = frame(&mut lit, 5.0 + 3.17 * k as f32, TorchFlicker::STYLE);
                for (m, (a, b)) in moved.iter_mut().zip(f.iter().zip(&base)) {
                    *m |= a != b;
                }
            }
            // A pixel's ray (`R_ViewChanged`'s centre and scale at fov 90,
            // square pixels) to its first solid point, by halving.
            let (yr, up) = (yaw.to_radians(), [0.0, 0.0, 1.0]);
            let (fwd, right) = ([yr.cos(), yr.sin(), 0.0], [yr.sin(), -yr.cos(), 0.0]);
            let sees_a_torch = |p: usize| {
                let (sx, sy) = (((p % w) as f32 - 159.5) / 160.0, ((p / w) as f32 - 99.5) / 160.0);
                let (d, _) = normalize(add(fwd, add(scale(right, sx), scale(up, -sy))));
                let at = |t: f32| mul_add(eye, t, d);
                let (mut lo, mut hi) = (0.0f32, 4096.0f32);
                if test_line(&bsp, eye, at(hi)) {
                    return true; // nothing solid: nothing to judge
                }
                for _ in 0..24 {
                    let mid = 0.5 * (lo + hi);
                    if test_line(&bsp, eye, at(mid)) {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                let q = at(lo - 2.0);
                torches
                    .iter()
                    .any(|t| dot(sub(t.origin, q), sub(t.origin, q)).sqrt() < t.light && test_line(&bsp, t.origin, q))
            };
            let flickering: Vec<usize> = (0..w * h).filter(|&p| moved[p]).collect();
            let through = flickering.iter().filter(|&&p| !sees_a_torch(p)).count();
            (flickering.len(), through)
        };
        let (worst, through) = audit([864.0, -312.0, 464.0], 90.0);
        assert!(through * 100 <= w * h, "the worst view: {through} of its {worst} flickering pixels see no torch");
        let (arch, through) = audit([1488.0, 1240.0, 296.0], 270.0);
        assert!(arch > w * h / 20, "the arch's flames light it ({arch} pixels)");
        assert!(through * 100 <= arch * 5, "the arch: {through} of its {arch} flickering pixels see no torch");
        eprintln!("worst view: {worst} pixels flicker; the arch: {arch}, {through} unseen");
    }

    /// The floor's lightmap at `time`, with the torches at `strength` (style
    /// 0 at id's 'm').
    fn floor_luxels(bsp: &Bsp, set: &mut TorchSet, time: f32, strength: TorchFlicker) -> Vec<f32> {
        let mut styles = crate::render::NEUTRAL_LIGHTSTYLE_SCALES;
        styles[0] = 264.0 / 256.0;
        set.animate(time, Smooth, strength);
        let mut poly = Vec::new();
        assert!(face_world_poly(bsp, &bsp.faces[0], &mut poly));
        let lm = face_lightmap_with(bsp, &bsp.faces[0], &poly, &styles, set.face(0), &[], 0).expect("lit");
        owned(&lm)
    }

    fn owned(lm: &LightMap) -> Vec<f32> {
        match &lm.luxels {
            crate::render::light::Luxels::Owned(v) => v.clone(),
            crate::render::light::Luxels::Static(s) => s.iter().map(|&b| f32::from(b)).collect(),
        }
    }

    /// Over the flicker's run a torch-lit face's light averages to id's:
    /// every luxel within a hundredth of a luxel unit, the colormap row
    /// (`blocklights`) within one; at strength 0 it is id's bit for bit.
    #[test]
    fn the_time_average_is_ids_light() {
        let bsp = torch_room(&lump(&[FLAME]), 120);
        let mut set = TorchSet::build(&bsp, 1);
        let id = floor_luxels(&bsp, &mut set, 0.0, TorchFlicker::OFF);
        assert!(id.iter().all(|&l| l == 120.0 * 264.0 / 256.0), "strength 0 is id's");
        // The big flame's two voices (4.6 s and 2.83 s) come round together
        // every 391 s: that, at 100 Hz.
        let n = 39_100;
        let mut sum = vec![0.0f64; id.len()];
        let mut moved = 0usize;
        for f in 0..n {
            let l = floor_luxels(&bsp, &mut set, f as f32 / 100.0, TorchFlicker::STYLE);
            moved += usize::from(l != id);
            for (a, &v) in sum.iter_mut().zip(&l) {
                *a += f64::from(v);
            }
        }
        assert!(moved > n * 9 / 10, "the light moves ({moved} of {n} frames)");
        let worst = sum.iter().zip(&id).map(|(&a, &want)| (a / n as f64 - f64::from(want)).abs()).fold(0.0, f64::max);
        assert!(worst < 0.01, "a luxel's average is {worst} from id's");
        // As colormap rows: (65280 - luxel*256) >> 10, averaged.
        let row = |l: f64| ((65280.0 - (l * 256.0).round()) as i64 >> 10) as f64;
        for (&a, &want) in sum.iter().zip(&id) {
            assert!((row(a / n as f64) - row(f64::from(want))).abs() <= 1.0);
        }
    }

    /// A luxel the torches drive below nothing clamps at black (the darkest
    /// colormap row) and one past the brightest at the colormap's ceiling
    /// (row 0): `R_BuildLightMap`'s bound, never a wrapped row.
    #[test]
    fn the_extremes_clamp_at_black_and_at_the_ceiling() {
        let shares: Box<[f32]> = vec![0.0, 255.0, 1000.0, 255.0].into_boxed_slice();
        let lit = [TorchLit { torch: 0, shares }];
        let lm = |scale: f32, base: f32| {
            let torches = FaceTorches { lit: &lit, scales: &[scale] };
            let mut buf = vec![base; 4];
            torches.add_to(&mut buf, 264.0 / 256.0);
            let lm = LightMap { luxels: crate::render::light::Luxels::Owned(buf), lmw: 2, lmh: 2, texmins: [0.0; 2] };
            let mut bl = Vec::new();
            lm.blocklights_into(&mut bl);
            bl
        };
        // Pulled far below 0: black (t = 16320, row 63), never more.
        let dark = lm(-1.0, 10.0);
        assert_eq!(dark[0], (65280 - 2560) >> 2, "a luxel no torch lights is untouched");
        assert!(dark[1..].iter().all(|&t| t == 16320), "{dark:?}");
        // Pushed past white: the ceiling (t = 64, row 0), never less.
        let bright = lm(2.0, 200.0);
        assert!(bright[1..].iter().all(|&t| t == 64), "{bright:?}");
        assert!(bright.iter().chain(&dark).all(|&t| (64..=16320).contains(&t)));
    }

    fn scene_at<'a>(
        bsp: &'a Bsp,
        palette: &'a crate::render::Palette,
        colormap: &'a [u8],
        time: f32,
        torches: TorchFlicker,
    ) -> Scene<'a> {
        let cam = Camera::looking_at([0.0, -200.0, 0.0], [0.0, 100.0, -128.0], 90.0);
        let options = RenderOptions { video: VideoCvars { torches, ..VideoCvars::MODERN }, ..RenderOptions::default() };
        Scene { time, colormap: Some(colormap), options, ..Scene::new(bsp, cam, 160, 100, palette) }
    }

    fn row_colormap() -> Vec<u8> {
        (0..crate::render::light::COLORMAP_LEN).map(|i| (i / 256) as u8).collect()
    }

    /// With the extra on and no torch in reach the frame is id's byte for
    /// byte; with one in reach it moves, and a renderer that drew other
    /// times first draws each time as a fresh one does (the caches are keyed
    /// on the torches' scales): the flicker is a function of the time alone,
    /// so a demo and the live game show the same light at the same time.
    #[test]
    fn no_torch_in_reach_is_ids_frame_and_the_flicker_is_the_times() {
        let palette = crate::render::fixtures::ramp_palette();
        let cm = row_colormap();
        for far in [lump(&[]), lump(&["\"classname\" \"light_flame_large_yellow\" \"origin\" \"5000 0 0\""])] {
            let bsp = torch_room(&far, 120);
            let mut r = Renderer::new();
            for f in 0..40 {
                let t = f as f32 * 0.137;
                let on = r.render(&scene_at(&bsp, &palette, &cm, t, TorchFlicker::from_value(2.0)));
                let off = crate::render::fixtures::render_once(&scene_at(&bsp, &palette, &cm, t, TorchFlicker::OFF));
                assert!(on.pixels == off.pixels, "frame {f}: id's");
            }
        }
        let bsp = torch_room(&lump(&[FLAME]), 120);
        let off = crate::render::fixtures::render_once(&scene_at(&bsp, &palette, &cm, 1.0, TorchFlicker::OFF));
        let mut warm = Renderer::new();
        let mut differs = 0;
        for f in 0..60 {
            let t = 7.0 + f as f32 / 60.0;
            let a = warm.render(&scene_at(&bsp, &palette, &cm, t, TorchFlicker::STYLE));
            let fresh = crate::render::fixtures::render_once(&scene_at(&bsp, &palette, &cm, t, TorchFlicker::STYLE));
            assert!(a.pixels == fresh.pixels, "t = {t}: the warm renderer's frame is the fresh one's");
            differs += usize::from(a.pixels != off.pixels);
        }
        assert!(differs > 30, "the flame flickers ({differs} of 60)");
        // Off again: id's frame from the same renderer.
        let back = warm.render(&scene_at(&bsp, &palette, &cm, 1.0, TorchFlicker::OFF));
        assert!(back.pixels == off.pixels);
    }

    /// id's maps: e1m2's 24 steady torches and flames all light faces; start's
    /// 19 flickering wall torches (styles 1 and 6) are left to their styles,
    /// its 22 steady ones flicker; e1m1 has none.
    #[test]
    fn ids_maps_steady_torches() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../quake-data/ID1/PAK0.PAK");
        let Ok(pak) = crate::pak::Pak::open(&path) else {
            eprintln!("skipped: no shareware pak at {}", path.display());
            return;
        };
        let bsp = |m: &str| Bsp::parse(&pak.read_file(&format!("maps/{m}.bsp")).expect("read").expect(m)).expect(m);
        for (map, torches) in [("e1m1", 0), ("e1m2", 24), ("start", 22)] {
            let world = bsp(map);
            let set = TorchSet::build(&world, 1);
            let (n, pairs) = set.counts();
            assert_eq!(n, torches, "{map}");
            let lit: std::collections::BTreeSet<u32> =
                (0..world.faces.len()).flat_map(|f| set.face(f).lit.iter().map(|l| l.torch)).collect();
            assert_eq!(lit.len(), n, "{map}: every torch lights a face");
            assert_eq!(pairs == 0, n == 0, "{map}");
        }
        let start = TorchSet::build(&bsp("start"), 1);
        assert_eq!(start.torches().iter().filter(|t| t.kind == TorchKind::LargeFlame).count(), 10);
    }
}
