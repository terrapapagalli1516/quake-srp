//! Test fixtures shared by the renderer and 2-D modules' tests.

use crate::bsp::Bsp;
use crate::math::Vec3;
use crate::wad::Qpic;
use super::{demo_room, GEOM_CACHE, LIGHT_CACHE, SURF_CACHE};

/// Build a tiny but valid single-skin single-frame MDL whose frame-0
/// triangle, after the model->world transform, sits in front of the camera.
/// Uses a large `scale` so the decoded vertices span a visible extent.
pub(super) fn tiny_mdl() -> crate::mdl::Mdl {
    use crate::mdl::{AliasFrame, Frame, Mdl, MdlHeader, Skin, StVert, Triangle, TriVertex};
    let header = MdlHeader {
        ident: i32::from_le_bytes(*b"IDPO"),
        version: 6,
        // 1 unit of v -> 1 world unit; origin shifts to centre the box.
        scale: [1.0, 1.0, 1.0],
        scale_origin: [-16.0, -16.0, -16.0],
        boundingradius: 32.0,
        eyeposition: [0.0, 0.0, 0.0],
        numskins: 1,
        skinwidth: 1,
        skinheight: 1,
        numverts: 3,
        numtris: 1,
        numframes: 1,
        synctype: 0,
        flags: 0,
        size: 1.0,
    };
    let verts = vec![
        TriVertex { v: [0, 0, 0], lightnormalindex: 0 },
        TriVertex { v: [32, 0, 0], lightnormalindex: 0 },
        TriVertex { v: [0, 0, 32], lightnormalindex: 0 },
    ];
    Mdl {
        header,
        skins: vec![Skin::Single(vec![0])],
        stverts: vec![StVert { onseam: 0, s: 0, t: 0 }; 3],
        triangles: vec![Triangle { facesfront: 1, vertindex: [0, 1, 2] }],
        frames: vec![Frame::Single(AliasFrame {
            name: "f0".into(),
            bboxmin: TriVertex { v: [0, 0, 0], lightnormalindex: 0 },
            bboxmax: TriVertex { v: [32, 0, 32], lightnormalindex: 0 },
            verts,
        })],
    }
}

/// Build a one-face BSP with the given lighting lump, lightofs, and flags,
/// plus a 32x32 (=> 3x3 luxel) world polygon.
pub(super) fn one_face_bsp(
    lighting: Vec<u8>,
    lightofs: i32,
    flags: i32,
) -> (Bsp, crate::bsp::DFace, Vec<Vec3>) {
    let mut bsp = demo_room();
    // Replace texinfo[0] with an axis-aligned one and clear textures so the
    // texinfo lookup in face_lightmap resolves predictably.
    bsp.texinfo = vec![crate::bsp::TexInfo {
        vecs: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]],
        miptex: 0,
        flags,
    }];
    bsp.lighting = lighting;
    let face = crate::bsp::DFace {
        planenum: 0,
        side: 0,
        firstedge: 0,
        numedges: 4,
        texinfo: 0,
        styles: [0, 0, 0, 0],
        lightofs,
    };
    let poly = vec![
        [0.0, 0.0, 0.0],
        [32.0, 0.0, 0.0],
        [32.0, 32.0, 0.0],
        [0.0, 32.0, 0.0],
    ];
    (bsp, face, poly)
}

/// `one_face_bsp` with plane 0 forced to the z=0 surface plane (`normal
/// [0,0,1]`, `dist 0`) so a light's distance/impact math is predictable: the
/// 3x3-luxel face spans surface `(s,t)` in `0..=32` (texmins 0, axis-aligned
/// vecs), so luxel `(i,j)` lives at world `(16i, 16j, 0)`.
pub(super) fn one_face_bsp_zplane(luxel: u8) -> (Bsp, crate::bsp::DFace, Vec<Vec3>) {
    let (mut bsp, face, poly) = one_face_bsp(vec![luxel; 9], 0, 0);
    bsp.planes[0] = crate::bsp::DPlane {
        normal: [0.0, 0.0, 1.0],
        dist: 0.0,
        ptype: 0,
    };
    (bsp, face, poly)
}

/// A z-plane one-face BSP whose face uses two light styles. The LIGHTING
/// lump concatenates the two `3x3` luxel blocks: block for `styles[0]` first
/// (all `b0`), then `styles[1]` (all `b1`). `styles` are the style indices.
pub(super) fn two_style_face_bsp(
    styles: [u8; 4],
    b0: u8,
    b1: u8,
) -> (Bsp, crate::bsp::DFace, Vec<Vec3>) {
    // 9 luxels per block, two blocks concatenated.
    let mut lighting = vec![b0; 9];
    lighting.extend(std::iter::repeat(b1).take(9));
    let (mut bsp, mut face, poly) = one_face_bsp_zplane(b0);
    bsp.lighting = lighting;
    face.styles = styles;
    (bsp, face, poly)
}

/// Build a synthetic 64x64 "liquid" miptexture: a vivid gradient of palette
/// indices so a small change in the sampled (s,t) lands on a different index.
pub(super) fn synthetic_liquid_pixels() -> Vec<u8> {
    let mut px = vec![0u8; 64 * 64];
    for y in 0..64usize {
        for x in 0..64usize {
            // A non-trivial pattern: index depends on both axes.
            px[y * 64 + x] = ((x * 4 + y * 7) % 256) as u8;
        }
    }
    px
}

/// Build a synthetic 256x128 sky miptexture: the LEFT half (the alpha overlay)
/// is index 0 (transparent) in a band and a vivid index elsewhere; the RIGHT
/// half (the solid background) is a gradient. So compositing shows the
/// background through the transparent overlay band.
pub(super) fn synthetic_sky_pixels() -> Vec<u8> {
    let mut px = vec![0u8; 256 * 128];
    for y in 0..128usize {
        for x in 0..128usize {
            // Left (overlay) half: transparent (0) in the left third, else 200.
            px[y * 256 + x] = if x < 42 { 0 } else { 200 };
            // Right (background) half: a non-zero gradient, never 0.
            px[y * 256 + (128 + x)] = (1 + ((x + y) % 200)) as u8;
        }
    }
    px
}

/// A test palette where index `i` maps to the RGB `[i, i, i]` (so a texel's
/// palette index is recoverable from any channel of the drawn pixel). Index
/// 255 stays the transparent colour and is never blitted.
pub(super) fn ramp_palette() -> [[u8; 3]; 256] {
    let mut p = [[0u8; 3]; 256];
    for (i, px) in p.iter_mut().enumerate() {
        *px = [i as u8, i as u8, i as u8];
    }
    p
}

/// A 128x128 conchars atlas with EVERY glyph cell solidly filled (index 95),
/// so any drawn character paints recognisable pixels.
pub(super) fn solid_conchars() -> Qpic {
    Qpic { width: 128, height: 128, data: vec![95u8; 128 * 128] }
}

/// A solid `w*h` Qpic filled with palette index `idx`.
pub(super) fn solid_pic(w: i32, h: i32, idx: u8) -> crate::wad::Qpic {
    crate::wad::Qpic {
        width: w,
        height: h,
        data: vec![idx; (w * h) as usize],
    }
}

/// Clear both thread-local caches so a test starts from a known state
/// (tests share a thread, and a prior test may have populated them).
pub(super) fn reset_render_caches() {
    GEOM_CACHE.with(|c| *c.borrow_mut() = None);
    LIGHT_CACHE.with(|c| *c.borrow_mut() = None);
    SURF_CACHE.with(|c| *c.borrow_mut() = None);
}

/// A `demo_room` whose every face is a 2-style lightmapped wall (styles
/// `[0, 1]`) pointing into a uniform lighting lump. Rendering this with a
/// non-neutral style-1 scale forces the OWNED multi-style combine on every
/// face -> exercises the lightmap surface cache end-to-end.
pub(super) fn lightmapped_demo_room(block0: u8, block1: u8) -> Bsp {
    let mut bsp = demo_room();
    // Two concatenated blocks per face, uniform so any face's grid (whatever
    // its extents) reads well-defined bytes. Lump is large enough for the
    // biggest face's 2*lmw*lmh.
    let mut lighting = vec![block0; 200_000];
    for b in lighting.iter_mut().skip(100_000) {
        *b = block1;
    }
    bsp.lighting = lighting;
    for f in bsp.faces.iter_mut() {
        f.lightofs = 0;
        f.styles = [0, 1, 255, 255];
    }
    bsp
}

/// A 64x64 backtile whose texel (x, y) is palette index `(x + 64*y) % 251`,
/// so any sampling error shows up as the wrong colour.
pub(super) fn test_backtile() -> Qpic {
    let data = (0..64 * 64).map(|i| (i % 251) as u8).collect();
    Qpic { width: 64, height: 64, data }
}
