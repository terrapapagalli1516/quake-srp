//! The affine triangle/span filler alias models are drawn with.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/d_polyse.c` — `D_PolysetDraw`, `D_RasterizeAliasPolySmooth`,
//! `D_PolysetCalcGradients`, `D_PolysetDrawSpans8`; `adivtab.h`.

use super::alias::{AliasSetup, FinalVert, ALIAS_ONSEAM};
use super::band::Band;

/// The sentinel `D_RasterizeAliasPolySmooth` stores in a span's `count`.
const SPAN_END: i32 = -999_999;

/// `spanpackage_t` (d_polyse.c), with the destination and z pointers as one
/// framebuffer index and the skin pointer as a skin index.
#[derive(Clone, Copy, Default)]
struct SpanPackage {
    pdest: isize,
    count: i32,
    ptex: isize,
    sfrac: i32,
    tfrac: i32,
    light: i32,
    zi: i32,
}

/// `edgetables` (d_polyse.c): per vertex ordering, the left and right edge
/// chains (indices into `r_p0/r_p1/r_p2`) and their edge counts.
const POLY_EDGETABLES: [(usize, [usize; 3], usize, [usize; 3]); 12] = [
    (1, [0, 2, 0], 2, [0, 1, 2]),
    (2, [1, 0, 2], 1, [1, 2, 0]),
    (1, [0, 2, 0], 1, [1, 2, 0]),
    (1, [1, 0, 0], 2, [1, 2, 0]),
    (2, [0, 2, 1], 1, [0, 1, 0]),
    (1, [2, 1, 0], 1, [2, 0, 0]),
    (1, [2, 1, 0], 2, [2, 0, 1]),
    (2, [2, 1, 0], 1, [2, 0, 0]),
    (1, [1, 0, 0], 1, [1, 2, 0]),
    (1, [2, 1, 0], 1, [0, 1, 0]),
    (1, [1, 0, 0], 1, [2, 0, 0]),
    (1, [0, 2, 0], 1, [0, 1, 0]),
];

/// `FloorDivMod` (mathlib.c) for the long edges `adivtab` does not cover.
fn floor_div_mod(numer: f64, denom: f64) -> (i32, i32) {
    if denom <= 0.0 {
        return (0, 0); // the C Sys_Errors; a zero-height edge draws nothing
    }
    if numer >= 0.0 {
        let x = (numer / denom).floor();
        (x as i32, (numer - x * denom).floor() as i32)
    } else {
        let x = (-numer / denom).floor();
        let mut q = -(x as i32);
        let mut r = (-numer - x * denom).floor() as i32;
        if r != 0 {
            q -= 1;
            r = denom as i32 - r;
        }
        (q, r)
    }
}

/// `(int)` of a float the way the x86 id built for does it (`fistp` /
/// `cvttss2si`): truncation toward zero, and the "integer indefinite"
/// 0x80000000 for a NaN or a value out of range, where Rust's `as` saturates
/// (a huge positive step would come out 0x7FFFFFFF, id's 0x80000000). Sliver
/// triangles out of `R_AliasClipTriangle` reach this: a denominator of 1 or 2
/// under a long edge makes a 1/z step of 1e11.
pub(super) fn c_ftoi(x: f64) -> i32 {
    if x > -2_147_483_649.0 && x < 2_147_483_648.0 {
        x as i32
    } else {
        i32::MIN
    }
}

/// The framebuffer side of `D_PolysetDraw` (d_polyse.c): a band of the view
/// and its z-buffer, and the rasteriser state the C keeps in globals.
///
/// A triangle is walked whole, as id walks it — its edges, its span
/// packages, every step — and only the pixels in the band's rows are
/// written, so each band of a view draws exactly its share of what one pass
/// over the whole view draws.
pub(super) struct PolyFramebuffer<'b, 'a> {
    band: &'b mut Band<'a>,
    width: isize,
    // D_PolysetSetUpForLineScan
    errorterm: i32,
    erroradjustup: i32,
    erroradjustdown: i32,
    ubasestep: i32,
    // D_PolysetCalcGradients
    r_lstepx: i32,
    r_lstepy: i32,
    r_sstepx: i32,
    r_sstepy: i32,
    r_tstepx: i32,
    r_tstepy: i32,
    r_zistepx: i32,
    r_zistepy: i32,
    a_sstepxfrac: i32,
    a_tstepxfrac: i32,
    a_ststepxwhole: isize,
    // the left-edge walk
    d_aspancount: i32,
    d_countextrastep: i32,
    d_pdest: isize,
    d_ptex: isize,
    d_sfrac: i32,
    d_tfrac: i32,
    d_light: i32,
    d_zi: i32,
    d_pdestbasestep: isize,
    d_pdestextrastep: isize,
    d_ptexbasestep: isize,
    d_ptexextrastep: isize,
    d_sfracbasestep: i32,
    d_sfracextrastep: i32,
    d_tfracbasestep: i32,
    d_tfracextrastep: i32,
    d_lightbasestep: i32,
    d_lightextrastep: i32,
    d_zibasestep: i32,
    d_ziextrastep: i32,
    spans: Vec<SpanPackage>,
    next_span: usize,
}

impl<'b, 'a> PolyFramebuffer<'b, 'a> {
    /// The band under the rasteriser, for the sprites `R_DrawEntitiesOnList`
    /// draws between two models (the rasteriser's state carries on past
    /// them, as id's globals do).
    pub(super) fn band(&mut self) -> &mut Band<'a> {
        self.band
    }

    /// The rasteriser over `band`.
    pub(super) fn new(band: &'b mut Band<'a>) -> PolyFramebuffer<'b, 'a> {
        let width = band.width() as isize;
        PolyFramebuffer {
            band,
            width,
            errorterm: 0,
            erroradjustup: 0,
            erroradjustdown: 0,
            ubasestep: 0,
            r_lstepx: 0,
            r_lstepy: 0,
            r_sstepx: 0,
            r_sstepy: 0,
            r_tstepx: 0,
            r_tstepy: 0,
            r_zistepx: 0,
            r_zistepy: 0,
            a_sstepxfrac: 0,
            a_tstepxfrac: 0,
            a_ststepxwhole: 0,
            d_aspancount: 0,
            d_countextrastep: 0,
            d_pdest: 0,
            d_ptex: 0,
            d_sfrac: 0,
            d_tfrac: 0,
            d_light: 0,
            d_zi: 0,
            d_pdestbasestep: 0,
            d_pdestextrastep: 0,
            d_ptexbasestep: 0,
            d_ptexextrastep: 0,
            d_sfracbasestep: 0,
            d_sfracextrastep: 0,
            d_tfracbasestep: 0,
            d_tfracextrastep: 0,
            d_lightbasestep: 0,
            d_lightextrastep: 0,
            d_zibasestep: 0,
            d_ziextrastep: 0,
            // DPS_MAXSPANS: one package per scanline of a triangle, plus the
            // end marker; grown as triangles need them.
            spans: Vec::new(),
            next_span: 0,
        }
    }

    /// The z test and write of one alias pixel: `D_PolysetDraw`'s
    /// `if ((lzi >> 16) >= *lpz) { *lpz = lzi >> 16; ... }` against id's
    /// 16-bit z-buffer of `(1/z * 0x8000 * 0x10000) >> 16`.
    /// Only a pixel in the band's rows is touched.
    #[inline]
    fn plot(&mut self, idx: isize, zi: i32, pal_index: u8, setup: &AliasSetup) {
        let Ok(i) = usize::try_from(idx) else { return };
        let Some((p, z)) = self.band.at(i) else { return };
        let z16 = zi >> 16;
        if z16 >= *z as i32 {
            *z = z16 as i16;
            *p = if setup.skin.is_some() { pal_index } else { setup.flat };
        }
    }

    /// `acolormap[texel + (light & 0xFF00)]` for the skin texel at `ptex`.
    #[inline]
    fn shade(setup: &AliasSetup, ptex: isize, light: i32) -> u8 {
        let texel = setup
            .skin
            .and_then(|s| usize::try_from(ptex).ok().and_then(|i| s.get(i)))
            .copied()
            .unwrap_or(0);
        match setup.colormap {
            Some(cm) => cm.get(texel as usize + (light & 0xFF00) as usize).copied().unwrap_or(texel),
            None => texel,
        }
    }

    /// `D_PolysetDrawFinalVerts` (d_polyse.c): the vertices of a subdivided
    /// model, drawn as points first (those inside the view: the caller's
    /// test, `R_AliasPrepareUnclippedPoints`).
    pub(super) fn draw_final_verts(&mut self, setup: &AliasSetup, fverts: &[FinalVert]) {
        for fv in fverts {
            let ptex = (fv.v[3] >> 16) as isize * setup.skinwidth as isize + (fv.v[2] >> 16) as isize;
            let pix = Self::shade(setup, ptex, fv.v[4]);
            self.plot(fv.v[1] as isize * self.width + fv.v[0] as isize, fv.v[5], pix, setup);
        }
    }

    /// `D_PolysetDraw` for one triangle: `D_DrawSubdiv` or `D_DrawNonSubdiv`,
    /// both of which skip back faces and move the back-facing seam vertices to
    /// the skin's back half.
    pub(super) fn polyset_draw(&mut self, setup: &AliasSetup, v: [FinalVert; 3], facesfront: bool) {
        let d_xdenom = (v[0].v[1] - v[1].v[1])
            .wrapping_mul(v[0].v[0] - v[2].v[0])
            .wrapping_sub((v[0].v[0] - v[1].v[0]).wrapping_mul(v[0].v[1] - v[2].v[1]));
        if d_xdenom >= 0 {
            return;
        }
        let mut p = [v[0].v, v[1].v, v[2].v];
        if !facesfront {
            for (pi, vi) in p.iter_mut().zip(v.iter()) {
                if vi.flags & ALIAS_ONSEAM != 0 {
                    pi[2] += setup.seamfixup;
                }
            }
        }
        if setup.subdiv {
            // D_DrawSubdiv: one light for the whole triangle (vertex 0's).
            let light = v[0].v[4] & 0xFF00;
            self.recursive_triangle(setup, light, p[0], p[1], p[2], 0);
        } else {
            self.rasterize_smooth(setup, p, d_xdenom);
        }
    }

    /// `D_PolysetRecursiveTriangle` (d_polyse.c): split the longest-first edge
    /// until every edge is at most a pixel, plotting each split point on a
    /// leading edge.
    #[allow(clippy::too_many_arguments)]
    fn recursive_triangle(&mut self, setup: &AliasSetup, light: i32, lp1: [i32; 6], lp2: [i32; 6], lp3: [i32; 6], depth: u32) {
        if depth > 64 {
            return;
        }
        let far = |a: &[i32; 6], b: &[i32; 6]| {
            let du = b[0] - a[0];
            let dv = b[1] - a[1];
            !(-1..=1).contains(&du) || !(-1..=1).contains(&dv)
        };
        let (lp1, lp2, lp3) = if far(&lp1, &lp2) {
            (lp1, lp2, lp3)
        } else if far(&lp2, &lp3) {
            (lp2, lp3, lp1) // split2
        } else if far(&lp3, &lp1) {
            (lp3, lp1, lp2) // split3
        } else {
            return; // entire tri is filled
        };
        let mut new = [0i32; 6];
        for i in [0, 1, 2, 3, 5] {
            new[i] = lp1[i].wrapping_add(lp2[i]) >> 1; // C int: wraps
        }
        // draw the point if splitting a leading edge
        let leading = !(lp2[1] > lp1[1] || (lp2[1] == lp1[1] && lp2[0] < lp1[0]));
        if leading {
            let ptex = (new[3] >> 16) as isize * setup.skinwidth as isize + (new[2] >> 16) as isize;
            let pix = Self::shade(setup, ptex, light);
            self.plot(new[1] as isize * self.width + new[0] as isize, new[5], pix, setup);
        }
        self.recursive_triangle(setup, light, lp3, lp1, new, depth + 1);
        self.recursive_triangle(setup, light, lp3, new, lp2, depth + 1);
    }

    /// `D_PolysetSetUpForLineScan` (d_polyse.c): Bresenham setup for an edge.
    fn setup_line_scan(&mut self, startu: i32, startv: i32, endu: i32, endv: i32) {
        self.errorterm = -1;
        let tm = endu - startu;
        let tn = endv - startv;
        if (-15..=16).contains(&tm) && (-15..=16).contains(&tn) {
            let (q, r) = ADIVTAB[(((tm + 15) << 5) + (tn + 15)) as usize];
            self.ubasestep = q;
            self.erroradjustup = r;
        } else {
            let (q, r) = floor_div_mod(tm as f64, tn as f64);
            self.ubasestep = q;
            self.erroradjustup = r;
        }
        self.erroradjustdown = tn;
    }

    /// `D_PolysetCalcGradients` (d_polyse.c): the per-pixel x and y steps of
    /// light, s, t and 1/z across the (affine) triangle. The C's `int`
    /// differences wrap and its `(int)` casts give 0x80000000 out of range
    /// ([`c_ftoi`]); the steps built from them wrap too (`left_edge_steps`,
    /// `scan_left_edge`, `draw_spans`), so a sliver draws what id's does.
    fn calc_gradients(&mut self, p: &[[i32; 6]; 3], d_xdenom: i32, skinwidth: i32) {
        let d = |a: i32, b: i32| a.wrapping_sub(b) as f64;
        let p00_minus_p20 = d(p[0][0], p[2][0]);
        let p01_minus_p21 = d(p[0][1], p[2][1]);
        let p10_minus_p20 = d(p[1][0], p[2][0]);
        let p11_minus_p21 = d(p[1][1], p[2][1]);
        let xstepdenominv = 1.0 / d_xdenom as f32 as f64;
        let ystepdenominv = -xstepdenominv;
        let step = |k: usize| {
            let t0 = d(p[0][k], p[2][k]);
            let t1 = d(p[1][k], p[2][k]);
            (
                (t1 * p01_minus_p21 - t0 * p11_minus_p21) * xstepdenominv,
                (t1 * p00_minus_p20 - t0 * p10_minus_p20) * ystepdenominv,
            )
        };
        // ceil() for light so positive steps are exaggerated, negative diminished
        let (lx, ly) = step(4);
        self.r_lstepx = c_ftoi(lx.ceil());
        self.r_lstepy = c_ftoi(ly.ceil());
        let (sx, sy) = step(2);
        self.r_sstepx = c_ftoi(sx);
        self.r_sstepy = c_ftoi(sy);
        let (tx, ty) = step(3);
        self.r_tstepx = c_ftoi(tx);
        self.r_tstepy = c_ftoi(ty);
        let (zx, zy) = step(5);
        self.r_zistepx = c_ftoi(zx);
        self.r_zistepy = c_ftoi(zy);
        self.a_sstepxfrac = self.r_sstepx & 0xFFFF;
        self.a_tstepxfrac = self.r_tstepx & 0xFFFF;
        // int arithmetic in the C: wraps like it
        self.a_ststepxwhole = skinwidth.wrapping_mul(self.r_tstepx >> 16).wrapping_add(self.r_sstepx >> 16) as isize;
    }

    /// The package for the current left-edge position.
    fn push_span(&mut self) {
        let sp = SpanPackage {
            pdest: self.d_pdest,
            count: self.d_aspancount,
            ptex: self.d_ptex,
            sfrac: self.d_sfrac,
            tfrac: self.d_tfrac,
            light: self.d_light,
            zi: self.d_zi,
        };
        // The packages are written in order from 0 (`next_span` never skips
        // one), so the next is at most one past the end.
        match self.spans.get_mut(self.next_span) {
            Some(slot) => *slot = sp,
            None => self.spans.push(sp),
        }
        self.next_span += 1;
    }

    /// Whether any pixel of the triangle `v` can fall in the band: its
    /// pixels lie between its vertices' rows and columns, so their view
    /// indices lie between the top-left and bottom-right corners' (the
    /// rasteriser's edges step between the vertices; a subdivided triangle's
    /// points are midpoints of them). A triangle wholly outside is another
    /// band's.
    pub(super) fn touches(&self, v: &[FinalVert; 3]) -> bool {
        let (mut umin, mut umax, mut vmin, mut vmax) = (i64::MAX, i64::MIN, i64::MAX, i64::MIN);
        for p in v {
            let (u, row) = (i64::from(p.v[0]), i64::from(p.v[1]));
            (umin, umax, vmin, vmax) = (umin.min(u), umax.max(u), vmin.min(row), vmax.max(row));
        }
        let w = self.width as i64;
        let own = self.band.indices();
        vmin * w + umin < own.end as i64 && vmax * w + umax >= own.start as i64
    }

    /// `D_PolysetScanLeftEdge` (d_polyse.c): walk `height` rows down the left
    /// edge, one span package per row.
    fn scan_left_edge(&mut self, height: i32, skinwidth: isize) {
        let mut height = height;
        loop {
            self.push_span();
            self.errorterm += self.erroradjustup;
            if self.errorterm >= 0 {
                self.d_pdest += self.d_pdestextrastep;
                self.d_aspancount += self.d_countextrastep;
                self.d_ptex += self.d_ptexextrastep;
                self.d_sfrac += self.d_sfracextrastep;
                self.d_ptex += (self.d_sfrac >> 16) as isize;
                self.d_sfrac &= 0xFFFF;
                self.d_tfrac += self.d_tfracextrastep;
                if self.d_tfrac & 0x10000 != 0 {
                    self.d_ptex += skinwidth;
                    self.d_tfrac &= 0xFFFF;
                }
                // C ints: a sliver's steps wrap them (0x80000000 steps)
                self.d_light = self.d_light.wrapping_add(self.d_lightextrastep);
                self.d_zi = self.d_zi.wrapping_add(self.d_ziextrastep);
                self.errorterm -= self.erroradjustdown;
            } else {
                self.d_pdest += self.d_pdestbasestep;
                self.d_aspancount += self.ubasestep;
                self.d_ptex += self.d_ptexbasestep;
                self.d_sfrac += self.d_sfracbasestep;
                self.d_ptex += (self.d_sfrac >> 16) as isize;
                self.d_sfrac &= 0xFFFF;
                self.d_tfrac += self.d_tfracbasestep;
                if self.d_tfrac & 0x10000 != 0 {
                    self.d_ptex += skinwidth;
                    self.d_tfrac &= 0xFFFF;
                }
                self.d_light = self.d_light.wrapping_add(self.d_lightbasestep);
                self.d_zi = self.d_zi.wrapping_add(self.d_zibasestep);
            }
            height -= 1;
            if height <= 0 {
                break;
            }
        }
    }

    /// Start a left edge at `top`: `d_ptex`/fractions (`frac`: keep the vertex's
    /// fractional s/t — the first edge does, the second restarts at 0 as in the
    /// C), light, 1/z and the destination.
    fn start_left_edge(&mut self, top: &[i32; 6], righttop_u: i32, skinwidth: isize, frac: bool) {
        self.d_aspancount = top[0] - righttop_u;
        self.d_ptex = (top[2] >> 16) as isize + (top[3] >> 16) as isize * skinwidth;
        if frac {
            self.d_sfrac = top[2] & 0xFFFF;
            self.d_tfrac = top[3] & 0xFFFF;
        } else {
            self.d_sfrac = 0;
            self.d_tfrac = 0;
        }
        self.d_light = top[4];
        self.d_zi = top[5];
        self.d_pdest = top[1] as isize * self.width + top[0] as isize;
    }

    /// The left-edge steps for the edge just set up by `setup_line_scan`.
    fn left_edge_steps(&mut self, skinwidth: isize) {
        self.d_pdestbasestep = self.width + self.ubasestep as isize;
        self.d_pdestextrastep = self.d_pdestbasestep + 1;
        // for negative steps in x along left edge, bias toward overflow rather
        // than underflow
        let working_lstepx = if self.ubasestep < 0 { self.r_lstepx.wrapping_sub(1) } else { self.r_lstepx };
        self.d_countextrastep = self.ubasestep + 1;
        let sb = self.r_sstepy.wrapping_add(self.r_sstepx.wrapping_mul(self.ubasestep));
        let tb = self.r_tstepy.wrapping_add(self.r_tstepx.wrapping_mul(self.ubasestep));
        self.d_ptexbasestep = (sb >> 16) as isize + (tb >> 16) as isize * skinwidth;
        self.d_sfracbasestep = sb & 0xFFFF;
        self.d_tfracbasestep = tb & 0xFFFF;
        self.d_lightbasestep = self.r_lstepy.wrapping_add(working_lstepx.wrapping_mul(self.ubasestep));
        self.d_zibasestep = self.r_zistepy.wrapping_add(self.r_zistepx.wrapping_mul(self.ubasestep));
        let se = self.r_sstepy.wrapping_add(self.r_sstepx.wrapping_mul(self.d_countextrastep));
        let te = self.r_tstepy.wrapping_add(self.r_tstepx.wrapping_mul(self.d_countextrastep));
        self.d_ptexextrastep = (se >> 16) as isize + (te >> 16) as isize * skinwidth;
        self.d_sfracextrastep = se & 0xFFFF;
        self.d_tfracextrastep = te & 0xFFFF;
        self.d_lightextrastep = self.d_lightbasestep.wrapping_add(working_lstepx);
        self.d_ziextrastep = self.d_zibasestep.wrapping_add(self.r_zistepx);
    }

    /// `D_PolysetDrawSpans8` (d_polyse.c): walk the right edge down the span
    /// packages from `start` to the end marker, filling each row from the left
    /// edge to the right one.
    fn draw_spans(&mut self, setup: &AliasSetup, start: usize) {
        let skinwidth = setup.skinwidth as isize;
        let band = self.band.indices();
        let (first, end) = (band.start as isize, band.end as isize);
        let mut k = start;
        loop {
            let Some(sp) = self.spans.get(k).copied() else { return };
            let lcount = self.d_aspancount - sp.count;
            self.errorterm += self.erroradjustup;
            if self.errorterm >= 0 {
                self.d_aspancount += self.d_countextrastep;
                self.errorterm -= self.erroradjustdown;
            } else {
                self.d_aspancount += self.ubasestep;
            }
            // A span wholly outside the band's rows writes nothing here: its
            // pixels are another band's.
            if lcount > 0 && sp.pdest < end && sp.pdest.saturating_add(lcount as isize) > first {
                let (mut lpdest, mut lptex) = (sp.pdest, sp.ptex);
                let (mut lsfrac, mut ltfrac, mut llight, mut lzi) = (sp.sfrac, sp.tfrac, sp.light, sp.zi);
                for _ in 0..lcount {
                    let pix = Self::shade(setup, lptex, llight);
                    self.plot(lpdest, lzi, pix, setup);
                    lpdest += 1;
                    lzi = lzi.wrapping_add(self.r_zistepx);
                    llight = llight.wrapping_add(self.r_lstepx);
                    lptex += self.a_ststepxwhole;
                    lsfrac += self.a_sstepxfrac;
                    lptex += (lsfrac >> 16) as isize;
                    lsfrac &= 0xFFFF;
                    ltfrac += self.a_tstepxfrac;
                    if ltfrac & 0x10000 != 0 {
                        lptex += skinwidth;
                        ltfrac &= 0xFFFF;
                    }
                }
            }
            k += 1;
            match self.spans.get(k) {
                Some(next) if next.count != SPAN_END => {}
                _ => return,
            }
        }
    }

    /// `D_RasterizeAliasPolySmooth` + `D_PolysetSetEdgeTable` (d_polyse.c):
    /// fill one screen triangle, top to bottom, left edge exclusive of the
    /// right.
    fn rasterize_smooth(&mut self, setup: &AliasSetup, p: [[i32; 6]; 3], d_xdenom: i32) {
        // D_PolysetSetEdgeTable
        let mut edgetableindex = 0usize;
        let table = 'table: {
            if p[0][1] >= p[1][1] {
                if p[0][1] == p[1][1] {
                    break 'table if p[0][1] < p[2][1] { 2 } else { 5 };
                }
                edgetableindex = 1;
            }
            if p[0][1] == p[2][1] {
                break 'table if edgetableindex != 0 { 8 } else { 9 };
            } else if p[1][1] == p[2][1] {
                break 'table if edgetableindex != 0 { 10 } else { 11 };
            }
            if p[0][1] > p[2][1] {
                edgetableindex += 2;
            }
            if p[1][1] > p[2][1] {
                edgetableindex += 4;
            }
            edgetableindex
        };
        let (numleft, left, numright, right) = POLY_EDGETABLES[table];
        let skinwidth = setup.skinwidth as isize;
        let plefttop = p[left[0]];
        let prighttop = p[right[0]];
        let pleftbottom = p[left[1]];
        let prightbottom = p[right[1]];
        let initialleftheight = pleftbottom[1] - plefttop[1];
        let initialrightheight = prightbottom[1] - prighttop[1];
        if initialleftheight < 0 || initialrightheight < 0 {
            return;
        }

        self.calc_gradients(&p, d_xdenom, setup.skinwidth);

        // scan out the top (and possibly only) part of the left edge
        self.next_span = 0;
        self.start_left_edge(&plefttop, prighttop[0], skinwidth, true);
        if initialleftheight == 1 {
            self.push_span();
        } else if initialleftheight > 1 {
            self.setup_line_scan(plefttop[0], plefttop[1], pleftbottom[0], pleftbottom[1]);
            self.left_edge_steps(skinwidth);
            self.scan_left_edge(initialleftheight, skinwidth);
        }
        // scan out the bottom part of the left edge, if it exists
        if numleft == 2 {
            let top = pleftbottom;
            let bottom = p[left[2]];
            let height = bottom[1] - top[1];
            self.start_left_edge(&top, prighttop[0], skinwidth, false);
            if height == 1 {
                self.push_span();
            } else if height > 1 {
                self.setup_line_scan(top[0], top[1], bottom[0], bottom[1]);
                self.left_edge_steps(skinwidth);
                self.scan_left_edge(height, skinwidth);
            }
        }

        // scan out the top (and possibly only) part of the right edge,
        // updating the count field
        let need = self.next_span.max(initialrightheight as usize) + 2;
        if self.spans.len() < need {
            self.spans.resize(need, SpanPackage::default());
        }
        self.setup_line_scan(prighttop[0], prighttop[1], prightbottom[0], prightbottom[1]);
        self.d_aspancount = 0;
        self.d_countextrastep = self.ubasestep + 1;
        let irh = initialrightheight as usize;
        let originalcount = self.spans[irh].count;
        self.spans[irh].count = SPAN_END; // mark end of the spanpackages
        if irh > 0 {
            self.draw_spans(setup, 0);
        }

        // scan out the bottom part of the right edge, if it exists
        if numright == 2 {
            self.spans[irh].count = originalcount;
            self.d_aspancount = prightbottom[0] - prighttop[0];
            let top = prightbottom;
            let bottom = p[right[2]];
            let height = bottom[1] - top[1];
            if height <= 0 {
                return;
            }
            self.setup_line_scan(top[0], top[1], bottom[0], bottom[1]);
            self.d_countextrastep = self.ubasestep + 1;
            let end = irh + height as usize;
            if self.spans.len() <= end {
                self.spans.resize(end + 1, SpanPackage::default());
            }
            self.spans[end].count = SPAN_END;
            self.draw_spans(setup, irh);
        }
    }
}

/// `adivtab` (adivtab.h): `{quotient, remainder}` of numerator / denominator
/// for both in -15..=16, indexed `((num+15) << 5) + (den+15)` — the small-edge
/// shortcut of `D_PolysetSetUpForLineScan`.
#[rustfmt::skip]
static ADIVTAB: [(i32, i32); 1024] = [
    (1, 0), (1, -1), (1, -2), (1, -3), (1, -4), (1, -5), (1, -6), (1, -7), (2, -1), (2, -3), (3, 0), (3, -3), (5, 0), (7, -1), (15, 0), (0, 0),
    (-15, 0), (-8, 1), (-5, 0), (-4, 1), (-3, 0), (-3, 3), (-3, 6), (-2, 1), (-2, 3), (-2, 5), (-2, 7), (-2, 9), (-2, 11), (-2, 13), (-1, 0), (-1, 1),
    (0, -14), (1, 0), (1, -1), (1, -2), (1, -3), (1, -4), (1, -5), (1, -6), (2, 0), (2, -2), (2, -4), (3, -2), (4, -2), (7, 0), (14, 0), (0, 0),
    (-14, 0), (-7, 0), (-5, 1), (-4, 2), (-3, 1), (-3, 4), (-2, 0), (-2, 2), (-2, 4), (-2, 6), (-2, 8), (-2, 10), (-2, 12), (-1, 0), (-1, 1), (-1, 2),
    (0, -13), (0, -13), (1, 0), (1, -1), (1, -2), (1, -3), (1, -4), (1, -5), (1, -6), (2, -1), (2, -3), (3, -1), (4, -1), (6, -1), (13, 0), (0, 0),
    (-13, 0), (-7, 1), (-5, 2), (-4, 3), (-3, 2), (-3, 5), (-2, 1), (-2, 3), (-2, 5), (-2, 7), (-2, 9), (-2, 11), (-1, 0), (-1, 1), (-1, 2), (-1, 3),
    (0, -12), (0, -12), (0, -12), (1, 0), (1, -1), (1, -2), (1, -3), (1, -4), (1, -5), (2, 0), (2, -2), (3, 0), (4, 0), (6, 0), (12, 0), (0, 0),
    (-12, 0), (-6, 0), (-4, 0), (-3, 0), (-3, 3), (-2, 0), (-2, 2), (-2, 4), (-2, 6), (-2, 8), (-2, 10), (-1, 0), (-1, 1), (-1, 2), (-1, 3), (-1, 4),
    (0, -11), (0, -11), (0, -11), (0, -11), (1, 0), (1, -1), (1, -2), (1, -3), (1, -4), (1, -5), (2, -1), (2, -3), (3, -2), (5, -1), (11, 0), (0, 0),
    (-11, 0), (-6, 1), (-4, 1), (-3, 1), (-3, 4), (-2, 1), (-2, 3), (-2, 5), (-2, 7), (-2, 9), (-1, 0), (-1, 1), (-1, 2), (-1, 3), (-1, 4), (-1, 5),
    (0, -10), (0, -10), (0, -10), (0, -10), (0, -10), (1, 0), (1, -1), (1, -2), (1, -3), (1, -4), (2, 0), (2, -2), (3, -1), (5, 0), (10, 0), (0, 0),
    (-10, 0), (-5, 0), (-4, 2), (-3, 2), (-2, 0), (-2, 2), (-2, 4), (-2, 6), (-2, 8), (-1, 0), (-1, 1), (-1, 2), (-1, 3), (-1, 4), (-1, 5), (-1, 6),
    (0, -9), (0, -9), (0, -9), (0, -9), (0, -9), (0, -9), (1, 0), (1, -1), (1, -2), (1, -3), (1, -4), (2, -1), (3, 0), (4, -1), (9, 0), (0, 0),
    (-9, 0), (-5, 1), (-3, 0), (-3, 3), (-2, 1), (-2, 3), (-2, 5), (-2, 7), (-1, 0), (-1, 1), (-1, 2), (-1, 3), (-1, 4), (-1, 5), (-1, 6), (-1, 7),
    (0, -8), (0, -8), (0, -8), (0, -8), (0, -8), (0, -8), (0, -8), (1, 0), (1, -1), (1, -2), (1, -3), (2, 0), (2, -2), (4, 0), (8, 0), (0, 0),
    (-8, 0), (-4, 0), (-3, 1), (-2, 0), (-2, 2), (-2, 4), (-2, 6), (-1, 0), (-1, 1), (-1, 2), (-1, 3), (-1, 4), (-1, 5), (-1, 6), (-1, 7), (-1, 8),
    (0, -7), (0, -7), (0, -7), (0, -7), (0, -7), (0, -7), (0, -7), (0, -7), (1, 0), (1, -1), (1, -2), (1, -3), (2, -1), (3, -1), (7, 0), (0, 0),
    (-7, 0), (-4, 1), (-3, 2), (-2, 1), (-2, 3), (-2, 5), (-1, 0), (-1, 1), (-1, 2), (-1, 3), (-1, 4), (-1, 5), (-1, 6), (-1, 7), (-1, 8), (-1, 9),
    (0, -6), (0, -6), (0, -6), (0, -6), (0, -6), (0, -6), (0, -6), (0, -6), (0, -6), (1, 0), (1, -1), (1, -2), (2, 0), (3, 0), (6, 0), (0, 0),
    (-6, 0), (-3, 0), (-2, 0), (-2, 2), (-2, 4), (-1, 0), (-1, 1), (-1, 2), (-1, 3), (-1, 4), (-1, 5), (-1, 6), (-1, 7), (-1, 8), (-1, 9), (-1, 10),
    (0, -5), (0, -5), (0, -5), (0, -5), (0, -5), (0, -5), (0, -5), (0, -5), (0, -5), (0, -5), (1, 0), (1, -1), (1, -2), (2, -1), (5, 0), (0, 0),
    (-5, 0), (-3, 1), (-2, 1), (-2, 3), (-1, 0), (-1, 1), (-1, 2), (-1, 3), (-1, 4), (-1, 5), (-1, 6), (-1, 7), (-1, 8), (-1, 9), (-1, 10), (-1, 11),
    (0, -4), (0, -4), (0, -4), (0, -4), (0, -4), (0, -4), (0, -4), (0, -4), (0, -4), (0, -4), (0, -4), (1, 0), (1, -1), (2, 0), (4, 0), (0, 0),
    (-4, 0), (-2, 0), (-2, 2), (-1, 0), (-1, 1), (-1, 2), (-1, 3), (-1, 4), (-1, 5), (-1, 6), (-1, 7), (-1, 8), (-1, 9), (-1, 10), (-1, 11), (-1, 12),
    (0, -3), (0, -3), (0, -3), (0, -3), (0, -3), (0, -3), (0, -3), (0, -3), (0, -3), (0, -3), (0, -3), (0, -3), (1, 0), (1, -1), (3, 0), (0, 0),
    (-3, 0), (-2, 1), (-1, 0), (-1, 1), (-1, 2), (-1, 3), (-1, 4), (-1, 5), (-1, 6), (-1, 7), (-1, 8), (-1, 9), (-1, 10), (-1, 11), (-1, 12), (-1, 13),
    (0, -2), (0, -2), (0, -2), (0, -2), (0, -2), (0, -2), (0, -2), (0, -2), (0, -2), (0, -2), (0, -2), (0, -2), (0, -2), (1, 0), (2, 0), (0, 0),
    (-2, 0), (-1, 0), (-1, 1), (-1, 2), (-1, 3), (-1, 4), (-1, 5), (-1, 6), (-1, 7), (-1, 8), (-1, 9), (-1, 10), (-1, 11), (-1, 12), (-1, 13), (-1, 14),
    (0, -1), (0, -1), (0, -1), (0, -1), (0, -1), (0, -1), (0, -1), (0, -1), (0, -1), (0, -1), (0, -1), (0, -1), (0, -1), (0, -1), (1, 0), (0, 0),
    (-1, 0), (-1, 1), (-1, 2), (-1, 3), (-1, 4), (-1, 5), (-1, 6), (-1, 7), (-1, 8), (-1, 9), (-1, 10), (-1, 11), (-1, 12), (-1, 13), (-1, 14), (-1, 15),
    (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0),
    (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0), (0, 0),
    (-1, -14), (-1, -13), (-1, -12), (-1, -11), (-1, -10), (-1, -9), (-1, -8), (-1, -7), (-1, -6), (-1, -5), (-1, -4), (-1, -3), (-1, -2), (-1, -1), (-1, 0), (0, 0),
    (1, 0), (0, 1), (0, 1), (0, 1), (0, 1), (0, 1), (0, 1), (0, 1), (0, 1), (0, 1), (0, 1), (0, 1), (0, 1), (0, 1), (0, 1), (0, 1),
    (-1, -13), (-1, -12), (-1, -11), (-1, -10), (-1, -9), (-1, -8), (-1, -7), (-1, -6), (-1, -5), (-1, -4), (-1, -3), (-1, -2), (-1, -1), (-1, 0), (-2, 0), (0, 0),
    (2, 0), (1, 0), (0, 2), (0, 2), (0, 2), (0, 2), (0, 2), (0, 2), (0, 2), (0, 2), (0, 2), (0, 2), (0, 2), (0, 2), (0, 2), (0, 2),
    (-1, -12), (-1, -11), (-1, -10), (-1, -9), (-1, -8), (-1, -7), (-1, -6), (-1, -5), (-1, -4), (-1, -3), (-1, -2), (-1, -1), (-1, 0), (-2, -1), (-3, 0), (0, 0),
    (3, 0), (1, 1), (1, 0), (0, 3), (0, 3), (0, 3), (0, 3), (0, 3), (0, 3), (0, 3), (0, 3), (0, 3), (0, 3), (0, 3), (0, 3), (0, 3),
    (-1, -11), (-1, -10), (-1, -9), (-1, -8), (-1, -7), (-1, -6), (-1, -5), (-1, -4), (-1, -3), (-1, -2), (-1, -1), (-1, 0), (-2, -2), (-2, 0), (-4, 0), (0, 0),
    (4, 0), (2, 0), (1, 1), (1, 0), (0, 4), (0, 4), (0, 4), (0, 4), (0, 4), (0, 4), (0, 4), (0, 4), (0, 4), (0, 4), (0, 4), (0, 4),
    (-1, -10), (-1, -9), (-1, -8), (-1, -7), (-1, -6), (-1, -5), (-1, -4), (-1, -3), (-1, -2), (-1, -1), (-1, 0), (-2, -3), (-2, -1), (-3, -1), (-5, 0), (0, 0),
    (5, 0), (2, 1), (1, 2), (1, 1), (1, 0), (0, 5), (0, 5), (0, 5), (0, 5), (0, 5), (0, 5), (0, 5), (0, 5), (0, 5), (0, 5), (0, 5),
    (-1, -9), (-1, -8), (-1, -7), (-1, -6), (-1, -5), (-1, -4), (-1, -3), (-1, -2), (-1, -1), (-1, 0), (-2, -4), (-2, -2), (-2, 0), (-3, 0), (-6, 0), (0, 0),
    (6, 0), (3, 0), (2, 0), (1, 2), (1, 1), (1, 0), (0, 6), (0, 6), (0, 6), (0, 6), (0, 6), (0, 6), (0, 6), (0, 6), (0, 6), (0, 6),
    (-1, -8), (-1, -7), (-1, -6), (-1, -5), (-1, -4), (-1, -3), (-1, -2), (-1, -1), (-1, 0), (-2, -5), (-2, -3), (-2, -1), (-3, -2), (-4, -1), (-7, 0), (0, 0),
    (7, 0), (3, 1), (2, 1), (1, 3), (1, 2), (1, 1), (1, 0), (0, 7), (0, 7), (0, 7), (0, 7), (0, 7), (0, 7), (0, 7), (0, 7), (0, 7),
    (-1, -7), (-1, -6), (-1, -5), (-1, -4), (-1, -3), (-1, -2), (-1, -1), (-1, 0), (-2, -6), (-2, -4), (-2, -2), (-2, 0), (-3, -1), (-4, 0), (-8, 0), (0, 0),
    (8, 0), (4, 0), (2, 2), (2, 0), (1, 3), (1, 2), (1, 1), (1, 0), (0, 8), (0, 8), (0, 8), (0, 8), (0, 8), (0, 8), (0, 8), (0, 8),
    (-1, -6), (-1, -5), (-1, -4), (-1, -3), (-1, -2), (-1, -1), (-1, 0), (-2, -7), (-2, -5), (-2, -3), (-2, -1), (-3, -3), (-3, 0), (-5, -1), (-9, 0), (0, 0),
    (9, 0), (4, 1), (3, 0), (2, 1), (1, 4), (1, 3), (1, 2), (1, 1), (1, 0), (0, 9), (0, 9), (0, 9), (0, 9), (0, 9), (0, 9), (0, 9),
    (-1, -5), (-1, -4), (-1, -3), (-1, -2), (-1, -1), (-1, 0), (-2, -8), (-2, -6), (-2, -4), (-2, -2), (-2, 0), (-3, -2), (-4, -2), (-5, 0), (-10, 0), (0, 0),
    (10, 0), (5, 0), (3, 1), (2, 2), (2, 0), (1, 4), (1, 3), (1, 2), (1, 1), (1, 0), (0, 10), (0, 10), (0, 10), (0, 10), (0, 10), (0, 10),
    (-1, -4), (-1, -3), (-1, -2), (-1, -1), (-1, 0), (-2, -9), (-2, -7), (-2, -5), (-2, -3), (-2, -1), (-3, -4), (-3, -1), (-4, -1), (-6, -1), (-11, 0), (0, 0),
    (11, 0), (5, 1), (3, 2), (2, 3), (2, 1), (1, 5), (1, 4), (1, 3), (1, 2), (1, 1), (1, 0), (0, 11), (0, 11), (0, 11), (0, 11), (0, 11),
    (-1, -3), (-1, -2), (-1, -1), (-1, 0), (-2, -10), (-2, -8), (-2, -6), (-2, -4), (-2, -2), (-2, 0), (-3, -3), (-3, 0), (-4, 0), (-6, 0), (-12, 0), (0, 0),
    (12, 0), (6, 0), (4, 0), (3, 0), (2, 2), (2, 0), (1, 5), (1, 4), (1, 3), (1, 2), (1, 1), (1, 0), (0, 12), (0, 12), (0, 12), (0, 12),
    (-1, -2), (-1, -1), (-1, 0), (-2, -11), (-2, -9), (-2, -7), (-2, -5), (-2, -3), (-2, -1), (-3, -5), (-3, -2), (-4, -3), (-5, -2), (-7, -1), (-13, 0), (0, 0),
    (13, 0), (6, 1), (4, 1), (3, 1), (2, 3), (2, 1), (1, 6), (1, 5), (1, 4), (1, 3), (1, 2), (1, 1), (1, 0), (0, 13), (0, 13), (0, 13),
    (-1, -1), (-1, 0), (-2, -12), (-2, -10), (-2, -8), (-2, -6), (-2, -4), (-2, -2), (-2, 0), (-3, -4), (-3, -1), (-4, -2), (-5, -1), (-7, 0), (-14, 0), (0, 0),
    (14, 0), (7, 0), (4, 2), (3, 2), (2, 4), (2, 2), (2, 0), (1, 6), (1, 5), (1, 4), (1, 3), (1, 2), (1, 1), (1, 0), (0, 14), (0, 14),
    (-1, 0), (-2, -13), (-2, -11), (-2, -9), (-2, -7), (-2, -5), (-2, -3), (-2, -1), (-3, -6), (-3, -3), (-3, 0), (-4, -1), (-5, 0), (-8, -1), (-15, 0), (0, 0),
    (15, 0), (7, 1), (5, 0), (3, 3), (3, 0), (2, 3), (2, 1), (1, 7), (1, 6), (1, 5), (1, 4), (1, 3), (1, 2), (1, 1), (1, 0), (0, 15),
    (-2, -14), (-2, -12), (-2, -10), (-2, -8), (-2, -6), (-2, -4), (-2, -2), (-2, 0), (-3, -5), (-3, -2), (-4, -4), (-4, 0), (-6, -2), (-8, 0), (-16, 0), (0, 0),
    (16, 0), (8, 0), (5, 1), (4, 0), (3, 1), (2, 4), (2, 2), (2, 0), (1, 7), (1, 6), (1, 5), (1, 4), (1, 3), (1, 2), (1, 1), (1, 0),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::alias::ALIAS_ZISCALE;
    use crate::render::Image;
    use crate::render::light::{COLORMAP_LEN, COLORMAP_ROWS};

    // -- Alias models: R_AliasSetupLighting + D_PolysetDraw --

    #[test]
    fn adivtab_is_floor_div_mod_for_downward_edges() {
        // D_PolysetSetUpForLineScan's table and FloorDivMod agree wherever the
        // C can reach FloorDivMod (a positive edge height).
        for tm in -15..=16 {
            for tn in 1..=16 {
                let t = ADIVTAB[(((tm + 15) << 5) + (tn + 15)) as usize];
                assert_eq!(t, floor_div_mod(tm as f64, tn as f64), "{tm}/{tn}");
            }
        }
    }

    /// Fill one screen triangle through `D_PolysetDraw`'s edge walker with a 1x1
    /// skin of `texel` and light `light` at every vertex.
    fn polyset_fill(img: &mut Image, verts: [(i32, i32); 3], texel: u8, light: i32, cm: Option<&[u8]>) {
        let skin = [texel];
        let setup = AliasSetup {
            transform: [[0.0; 4]; 3],
            r_ambientlight: 0,
            r_shadelight: 0.0,
            plightvec: [0.0; 3],
            ziscale: ALIAS_ZISCALE,
            subdiv: false,
            skin: Some(&skin),
            skinwidth: 1,
            seamfixup: 0,
            colormap: cm,
            flat: 0,
        };
        let fv = |(u, v): (i32, i32)| FinalVert { v: [u, v, 0, 0, light, 1 << 24], flags: 0 };
        let mut zbuf = vec![i16::MIN; img.w * img.h];
        let mut band = Band::whole(img.w, &mut img.pixels, &mut zbuf);
        let mut fb = PolyFramebuffer::new(&mut band);
        fb.polyset_draw(&setup, [fv(verts[0]), fv(verts[1]), fv(verts[2])], true);
    }

    #[test]
    fn polyset_triangles_tile_a_square_exactly_once() {
        // Two front-facing triangles splitting an 8x8 square cover its 64
        // pixels with no gap and no overlap, and nothing outside it: the fill
        // rule of D_RasterizeAliasPolySmooth (left edge in, right edge out, top
        // row in, bottom row out).
        let count = |img: &Image| img.pixels.iter().filter(|&&p| p != 0).count();
        let mut a = Image::new(12, 12, 0);
        polyset_fill(&mut a, [(2, 2), (10, 2), (10, 10)], 1, 0, None);
        let mut b = Image::new(12, 12, 0);
        polyset_fill(&mut b, [(2, 2), (10, 10), (2, 10)], 2, 0, None);
        assert_eq!((count(&a), count(&b)), (36, 28));
        for y in 0..12 {
            for x in 0..12 {
                let inside = (2..10).contains(&x) && (2..10).contains(&y);
                let (pa, pb) = (a.pixels[y * 12 + x] != 0, b.pixels[y * 12 + x] != 0);
                assert_eq!(pa || pb, inside, "({x},{y})");
                assert!(!(pa && pb), "overlap at ({x},{y})");
            }
        }
        // Back faces (d_xdenom >= 0) draw nothing.
        let mut c = Image::new(12, 12, 0);
        polyset_fill(&mut c, [(2, 2), (10, 10), (10, 2)], 1, 0, None);
        assert_eq!(count(&c), 0);
    }

    #[test]
    fn float_to_int_is_x86s_not_rusts_saturation() {
        // (int) on x86 truncates toward zero and gives 0x80000000 for NaN or
        // anything out of range, positive included.
        assert_eq!(c_ftoi(-2.7), -2);
        assert_eq!(c_ftoi(2.7), 2);
        assert_eq!(c_ftoi(2_147_483_647.0), i32::MAX);
        assert_eq!(c_ftoi(-2_147_483_648.9), i32::MIN);
        assert_eq!(c_ftoi(3.0e9), i32::MIN);
        assert_eq!(c_ftoi(-3.0e9), i32::MIN);
        assert_eq!(c_ftoi(f64::NAN), i32::MIN);
    }

    /// A sliver like the ones `R_AliasClipTriangle` leaves (review: 43 of
    /// 75,600 gun renders, e.g. v_rock2 frame 1 at 320x200): d_xdenom -4 under
    /// a 200-row edge whose ends are near and the middle vertex far. The 1/z
    /// x step is 5.4e10: id's `(int)` makes it 0x80000000, and the left-edge
    /// walk's `d_zi += d_ziextrastep` then wraps (a debug build panicked here).
    #[test]
    fn sliver_triangles_wrap_like_the_c_ints() {
        let skin = [1u8; 4];
        let setup = AliasSetup {
            transform: [[0.0; 4]; 3],
            r_ambientlight: 0,
            r_shadelight: 0.0,
            plightvec: [0.0; 3],
            ziscale: ALIAS_ZISCALE,
            subdiv: false,
            skin: Some(&skin),
            skinwidth: 2,
            seamfixup: 0,
            colormap: None,
            flat: 0,
        };
        let (near, far) = (1 << 30, 1 << 20);
        let fv = |u: i32, v: i32, zi: i32| FinalVert { v: [u, v, 0, 0, 0x7F00, zi], flags: 0 };
        let tri = [fv(0, 0, near), fv(4, 200, near), fv(2, 101, far)];
        let mut img = Image::new(8, 208, 0);
        let mut zbuf = vec![i16::MIN; img.w * img.h];
        let mut band = Band::whole(img.w, &mut img.pixels, &mut zbuf);
        let mut fb = PolyFramebuffer::new(&mut band);
        fb.polyset_draw(&setup, tri, true);
        let t = f64::from(near - far);
        assert_eq!((fb.r_zistepx, fb.r_zistepy), (i32::MIN, c_ftoi(-t)));
        assert_eq!(fb.d_zibasestep, c_ftoi(-t));
        assert_eq!(fb.d_ziextrastep, c_ftoi(-t).wrapping_add(i32::MIN));
    }

    #[test]
    fn polyset_pixels_go_through_the_colormap_row_of_the_light() {
        // acolormap[texel + (light & 0xFF00)]: row 40 of this colormap maps
        // texel 10 to 77, row 0 to itself (a fullbright texel stays itself).
        let mut cm = vec![0u8; COLORMAP_LEN];
        for row in 0..COLORMAP_ROWS {
            for col in 0..256 {
                cm[row * 256 + col] = if row == 40 && col == 10 { 77 } else { col as u8 };
            }
        }
        let mut img = Image::new(8, 8, 0);
        polyset_fill(&mut img, [(0, 0), (8, 0), (8, 8)], 10, (40 << 8) + 0x7F, Some(&cm));
        let drawn: Vec<u8> = img.pixels.iter().copied().filter(|&p| p != 0).collect();
        assert!(!drawn.is_empty());
        assert!(drawn.iter().all(|&i| i == 77), "{drawn:?}");
    }
}
