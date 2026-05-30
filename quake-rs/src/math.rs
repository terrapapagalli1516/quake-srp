//! Vector / matrix / angle math primitives.
//!
//! Ported from Quake's `WinQuake/mathlib.c` and `WinQuake/mathlib.h`
//! (plane field layout from `model.h`, angle indices from `quakedef.h`,
//! `signbits`/axial-type rules from `Mod_LoadPlanes` in `model.c`).
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//!
//! Faithfulness notes:
//! * This module OWNS [`Vec3`] (`lib.rs` re-exports it).
//! * C used out-parameters; here functions return values idiomatically.
//! * Where C used `double` for trig (`sin`/`cos`/`sqrt` are libm `double`
//!   functions and `DEG2RAD`/`anglemod` compute in `double`), we mirror that:
//!   compute in `f64`, then cast the result back to `f32`.
//! * Where C called `Sys_Error` (`FloorDivMod` bad denom, `BoxOnPlaneSide`
//!   bad signbits) we never panic: `floor_div_mod` returns `(0, 0)` and
//!   `box_on_plane_side` falls back to the general corner formula.

/// `vec3_t` — three single-precision floats. The math module owns this type.
pub type Vec3 = [f32; 3];

/// Angle index for pitch (`quakedef.h`: `#define PITCH 0`).
pub const PITCH: usize = 0;
/// Angle index for yaw (`quakedef.h`: `#define YAW 1`).
pub const YAW: usize = 1;
/// Angle index for roll (`quakedef.h`: `#define ROLL 2`).
pub const ROLL: usize = 2;

/// `M_PI` as used by `mathlib.h` (`f32::consts::PI`).
pub const M_PI: f32 = std::f32::consts::PI;

/// `DEG2RAD(a)` from `mathlib.c`: `( a * M_PI ) / 180.0F`.
///
/// The C macro multiplies the `float` angle by the `double` `M_PI` (3.14159...)
/// and divides by `180.0F`; the intermediate is `double`. We mirror that by
/// computing in `f64` using the C `M_PI` literal.
#[inline]
fn deg2rad(a: f32) -> f64 {
    // C: ( a * M_PI ) / 180.0F  — M_PI is the double 3.14159265358979323846.
    (f64::from(a) * std::f64::consts::PI) / 180.0
}

/// The runtime plane, mirroring `mplane_t` from `model.h`:
/// ```c
/// typedef struct mplane_s {
///     vec3_t normal;
///     float  dist;
///     byte   type;     // for texture axis selection and fast side tests
///     byte   signbits; // signx + signy<<1 + signz<<2
///     byte   pad[2];
/// } mplane_t;
/// ```
#[derive(Debug, Clone)]
pub struct Plane {
    pub normal: Vec3,
    pub dist: f32,
    /// `type` in C (0/1/2 if axial, else 3 = PLANE_ANYX). `type` is a keyword
    /// in Rust, so this field is named `ptype`.
    pub ptype: u8,
    /// `signbits`: bit `i` set when `normal[i] < 0`, exactly as `Mod_LoadPlanes`
    /// computes it.
    pub signbits: u8,
}

impl Plane {
    /// Build a plane from a normal and distance.
    ///
    /// `ptype` is `i` when the normal is axial along axis `i` (i.e. `normal[i]`
    /// is exactly `1.0` or `-1.0`), otherwise `3` (`PLANE_ANYX`). `signbits` is
    /// computed exactly as `Mod_LoadPlanes`: bit `j` is set iff `normal[j] < 0`.
    pub fn new(normal: Vec3, dist: f32) -> Plane {
        // Axial detection: a plane is axial (type 0/1/2) when its normal lies on
        // a coordinate axis, i.e. one component is +/-1 and the others are 0.
        let mut ptype: u8 = 3; // PLANE_ANYX
        for i in 0..3 {
            if normal[i] == 1.0 || normal[i] == -1.0 {
                ptype = i as u8;
                break;
            }
        }

        // signbits, exactly as Mod_LoadPlanes: bits |= 1<<j when normal[j] < 0.
        let mut signbits: u8 = 0;
        for j in 0..3 {
            signbits |= ((normal[j] < 0.0) as u8) << j;
        }

        Plane {
            normal,
            dist,
            ptype,
            signbits,
        }
    }
}

/// `DotProduct(x,y)` macro: `x[0]*y[0]+x[1]*y[1]+x[2]*y[2]`.
#[inline]
pub fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// `CrossProduct`.
#[inline]
pub fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// `VectorAdd(a,b,c)`.
#[inline]
pub fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// `VectorSubtract(a,b,c)`.
#[inline]
pub fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// `VectorScale(in, scale, out)`.
#[inline]
pub fn scale(v: Vec3, s: f32) -> Vec3 {
    [v[0] * s, v[1] * s, v[2] * s]
}

/// `VectorMA(veca, scale, vecb, vecc)`: `a + scale*b`.
#[inline]
pub fn mul_add(a: Vec3, scale: f32, b: Vec3) -> Vec3 {
    [
        a[0] + scale * b[0],
        a[1] + scale * b[1],
        a[2] + scale * b[2],
    ]
}

/// Alias for [`mul_add`], matching the C name `VectorMA`.
#[inline]
pub fn vector_ma(a: Vec3, scale: f32, b: Vec3) -> Vec3 {
    mul_add(a, scale, b)
}

/// `VectorInverse(v)`: negate each component.
#[inline]
pub fn inverse(v: Vec3) -> Vec3 {
    [-v[0], -v[1], -v[2]]
}

/// `VectorCompare(v1,v2)`: exact (bit-for-bit `==`) equality of all three
/// components, returning a bool (`1`/`0` in C).
#[inline]
pub fn compare(a: Vec3, b: Vec3) -> bool {
    a[0] == b[0] && a[1] == b[1] && a[2] == b[2]
}

/// `Length(v)`. C accumulates the squared length as `float`, then `sqrt`s in
/// `double` (libm) and stores back to `float`.
#[inline]
pub fn length(v: Vec3) -> f32 {
    let mut len: f32 = 0.0;
    for i in 0..3 {
        len += v[i] * v[i];
    }
    (f64::from(len)).sqrt() as f32
}

/// `VectorNormalize(v)`: returns the unit vector and the original length.
///
/// C mutates `v` in place and returns the length. Here we return
/// `(unit_vector, original_length)`. If the length is `0`, the original vector
/// is returned unchanged with length `0.0` (the C code leaves `v` untouched
/// when `length` is falsy).
#[inline]
pub fn normalize(v: Vec3) -> (Vec3, f32) {
    let len_sq = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
    let length = (f64::from(len_sq)).sqrt() as f32;

    if length != 0.0 {
        let ilength = 1.0 / length;
        ([v[0] * ilength, v[1] * ilength, v[2] * ilength], length)
    } else {
        (v, 0.0)
    }
}

/// `anglemod(a)`: the exact bit version from `mathlib.c`:
/// `(360.0/65536) * ((int)(a*(65536/360.0)) & 65535)`.
///
/// `65536/360.0` is a `double` constant; the multiply is `double`; the `(int)`
/// cast truncates toward zero (and is masked with `& 65535` as a 32-bit int).
pub fn anglemod(a: f32) -> f32 {
    // C: a = (360.0/65536) * ((int)(a*(65536/360.0)) & 65535);
    let scaled = f64::from(a) * (65536.0 / 360.0);
    let truncated = scaled as i32; // (int) cast: truncate toward zero
    let masked = truncated & 65535;
    ((360.0 / 65536.0) * f64::from(masked)) as f32
}

/// `AngleVectors(angles, forward, right, up)`.
///
/// Returns `(forward, right, up)`. Trig is done in `double` (libm `sin`/`cos`)
/// then cast back to `float`, matching the C. The deg→rad factor is
/// `M_PI*2 / 360` exactly as written.
pub fn angle_vectors(angles: Vec3) -> (Vec3, Vec3, Vec3) {
    // angle = angles[..] * (M_PI*2 / 360);  with M_PI the double 3.14159...
    let factor = std::f64::consts::PI * 2.0 / 360.0;

    let angle = f64::from(angles[YAW]) * factor;
    let sy = angle.sin();
    let cy = angle.cos();
    let angle = f64::from(angles[PITCH]) * factor;
    let sp = angle.sin();
    let cp = angle.cos();
    let angle = f64::from(angles[ROLL]) * factor;
    let sr = angle.sin();
    let cr = angle.cos();

    let forward: Vec3 = [(cp * cy) as f32, (cp * sy) as f32, (-sp) as f32];
    let right: Vec3 = [
        (-1.0 * sr * sp * cy + -1.0 * cr * -sy) as f32,
        (-1.0 * sr * sp * sy + -1.0 * cr * cy) as f32,
        (-1.0 * sr * cp) as f32,
    ];
    let up: Vec3 = [
        (cr * sp * cy + -sr * -sy) as f32,
        (cr * sp * sy + -sr * cy) as f32,
        (cr * cp) as f32,
    ];

    (forward, right, up)
}

/// `R_ConcatRotations(in1, in2, out)`: 3x3 * 3x3 matrix product.
pub fn concat_rotations(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    out[0][0] = a[0][0] * b[0][0] + a[0][1] * b[1][0] + a[0][2] * b[2][0];
    out[0][1] = a[0][0] * b[0][1] + a[0][1] * b[1][1] + a[0][2] * b[2][1];
    out[0][2] = a[0][0] * b[0][2] + a[0][1] * b[1][2] + a[0][2] * b[2][2];
    out[1][0] = a[1][0] * b[0][0] + a[1][1] * b[1][0] + a[1][2] * b[2][0];
    out[1][1] = a[1][0] * b[0][1] + a[1][1] * b[1][1] + a[1][2] * b[2][1];
    out[1][2] = a[1][0] * b[0][2] + a[1][1] * b[1][2] + a[1][2] * b[2][2];
    out[2][0] = a[2][0] * b[0][0] + a[2][1] * b[1][0] + a[2][2] * b[2][0];
    out[2][1] = a[2][0] * b[0][1] + a[2][1] * b[1][1] + a[2][2] * b[2][1];
    out[2][2] = a[2][0] * b[0][2] + a[2][1] * b[1][2] + a[2][2] * b[2][2];
    out
}

/// `R_ConcatTransforms(in1, in2, out)`: 3-row x 4-col affine transform product.
///
/// The fourth column is the translation; row `i`'s translation accumulates the
/// `in1[i][3]` term (`... + in1[i][3]`), exactly as the C does.
pub fn concat_transforms(a: &[[f32; 4]; 3], b: &[[f32; 4]; 3]) -> [[f32; 4]; 3] {
    let mut out = [[0.0f32; 4]; 3];
    out[0][0] = a[0][0] * b[0][0] + a[0][1] * b[1][0] + a[0][2] * b[2][0];
    out[0][1] = a[0][0] * b[0][1] + a[0][1] * b[1][1] + a[0][2] * b[2][1];
    out[0][2] = a[0][0] * b[0][2] + a[0][1] * b[1][2] + a[0][2] * b[2][2];
    out[0][3] = a[0][0] * b[0][3] + a[0][1] * b[1][3] + a[0][2] * b[2][3] + a[0][3];
    out[1][0] = a[1][0] * b[0][0] + a[1][1] * b[1][0] + a[1][2] * b[2][0];
    out[1][1] = a[1][0] * b[0][1] + a[1][1] * b[1][1] + a[1][2] * b[2][1];
    out[1][2] = a[1][0] * b[0][2] + a[1][1] * b[1][2] + a[1][2] * b[2][2];
    out[1][3] = a[1][0] * b[0][3] + a[1][1] * b[1][3] + a[1][2] * b[2][3] + a[1][3];
    out[2][0] = a[2][0] * b[0][0] + a[2][1] * b[1][0] + a[2][2] * b[2][0];
    out[2][1] = a[2][0] * b[0][1] + a[2][1] * b[1][1] + a[2][2] * b[2][1];
    out[2][2] = a[2][0] * b[0][2] + a[2][1] * b[1][2] + a[2][2] * b[2][2];
    out[2][3] = a[2][0] * b[0][3] + a[2][1] * b[1][3] + a[2][2] * b[2][3] + a[2][3];
    out
}

/// `BoxOnPlaneSide(emins, emaxs, p)`.
///
/// Returns `1`, `2`, or `3` (`1 + 2`). Faithful `signbits` switch for cases
/// `0..=7`. The C `default` case calls `Sys_Error`; to keep the function total
/// we instead use the general corner formula (the `#if 0` block): pick
/// `mins`/`maxs` per the sign of each normal component. (signbits is a `u8`, so
/// reaching the fallback requires bits >= 8, which `Plane::new` never produces.)
pub fn box_on_plane_side(emins: Vec3, emaxs: Vec3, p: &Plane) -> u8 {
    let n = p.normal;
    let (dist1, dist2): (f32, f32) = match p.signbits {
        0 => (
            n[0] * emaxs[0] + n[1] * emaxs[1] + n[2] * emaxs[2],
            n[0] * emins[0] + n[1] * emins[1] + n[2] * emins[2],
        ),
        1 => (
            n[0] * emins[0] + n[1] * emaxs[1] + n[2] * emaxs[2],
            n[0] * emaxs[0] + n[1] * emins[1] + n[2] * emins[2],
        ),
        2 => (
            n[0] * emaxs[0] + n[1] * emins[1] + n[2] * emaxs[2],
            n[0] * emins[0] + n[1] * emaxs[1] + n[2] * emins[2],
        ),
        3 => (
            n[0] * emins[0] + n[1] * emins[1] + n[2] * emaxs[2],
            n[0] * emaxs[0] + n[1] * emaxs[1] + n[2] * emins[2],
        ),
        4 => (
            n[0] * emaxs[0] + n[1] * emaxs[1] + n[2] * emins[2],
            n[0] * emins[0] + n[1] * emins[1] + n[2] * emaxs[2],
        ),
        5 => (
            n[0] * emins[0] + n[1] * emaxs[1] + n[2] * emins[2],
            n[0] * emaxs[0] + n[1] * emins[1] + n[2] * emaxs[2],
        ),
        6 => (
            n[0] * emaxs[0] + n[1] * emins[1] + n[2] * emins[2],
            n[0] * emins[0] + n[1] * emaxs[1] + n[2] * emaxs[2],
        ),
        7 => (
            n[0] * emins[0] + n[1] * emins[1] + n[2] * emins[2],
            n[0] * emaxs[0] + n[1] * emaxs[1] + n[2] * emaxs[2],
        ),
        _ => {
            // Bad signbits: instead of Sys_Error, fall back to the general
            // corner formula from the C `#if 0` block.
            let mut corner0: Vec3 = [0.0; 3];
            let mut corner1: Vec3 = [0.0; 3];
            for i in 0..3 {
                if n[i] < 0.0 {
                    corner0[i] = emins[i];
                    corner1[i] = emaxs[i];
                } else {
                    corner1[i] = emins[i];
                    corner0[i] = emaxs[i];
                }
            }
            (dot(n, corner0), dot(n, corner1))
        }
    };

    let mut sides: u8 = 0;
    if dist1 >= p.dist {
        sides = 1;
    }
    if dist2 < p.dist {
        sides |= 2;
    }
    sides
}

/// `ProjectPointOnPlane(dst, p, normal)`.
pub fn project_point_on_plane(p: Vec3, normal: Vec3) -> Vec3 {
    let inv_denom = 1.0 / dot(normal, normal);
    let d = dot(normal, p) * inv_denom;
    let n = [
        normal[0] * inv_denom,
        normal[1] * inv_denom,
        normal[2] * inv_denom,
    ];
    [p[0] - d * n[0], p[1] - d * n[1], p[2] - d * n[2]]
}

/// `PerpendicularVector(dst, src)`. Assumes `src` is normalized (as the C does).
pub fn perpendicular_vector(src: Vec3) -> Vec3 {
    // Find the smallest-magnitude axially aligned vector.
    let mut pos = 0usize;
    let mut minelem: f32 = 1.0;
    for i in 0..3 {
        if src[i].abs() < minelem {
            pos = i;
            minelem = src[i].abs();
        }
    }
    let mut tempvec: Vec3 = [0.0, 0.0, 0.0];
    tempvec[pos] = 1.0;

    // Project onto the plane defined by src, then normalize.
    let dst = project_point_on_plane(tempvec, src);
    let (unit, _len) = normalize(dst);
    unit
}

/// `RotatePointAroundVector(dst, dir, point, degrees)`.
///
/// Builds the change-of-basis matrix `m` (and its transpose `im`), a Z-rotation
/// by `degrees`, composes `m * zrot * im`, and applies it to `point`. Trig is
/// done in `double` and the rotation entries are stored as `float`, matching C.
pub fn rotate_point_around_vector(dir: Vec3, point: Vec3, degrees: f32) -> Vec3 {
    let vf = dir;
    let vr = perpendicular_vector(dir);
    let vup = cross(vr, vf);

    // m[row][col]
    let mut m = [[0.0f32; 3]; 3];
    m[0][0] = vr[0];
    m[1][0] = vr[1];
    m[2][0] = vr[2];

    m[0][1] = vup[0];
    m[1][1] = vup[1];
    m[2][1] = vup[2];

    m[0][2] = vf[0];
    m[1][2] = vf[1];
    m[2][2] = vf[2];

    // im = transpose(m), built the same way the C copies then swaps.
    let mut im = m;
    im[0][1] = m[1][0];
    im[0][2] = m[2][0];
    im[1][0] = m[0][1];
    im[1][2] = m[2][1];
    im[2][0] = m[0][2];
    im[2][1] = m[1][2];

    // zrot: Z rotation by `degrees`. C zeros it, sets diagonal to 1, then
    // overwrites the upper-left 2x2 block (zrot[2][2] stays 1).
    let mut zrot = [[0.0f32; 3]; 3];
    zrot[2][2] = 1.0;
    let rad = deg2rad(degrees); // double
    zrot[0][0] = rad.cos() as f32;
    zrot[0][1] = rad.sin() as f32;
    zrot[1][0] = (-rad.sin()) as f32;
    zrot[1][1] = rad.cos() as f32;

    let tmpmat = concat_rotations(&m, &zrot);
    let rot = concat_rotations(&tmpmat, &im);

    let mut dst: Vec3 = [0.0; 3];
    for i in 0..3 {
        dst[i] = rot[i][0] * point[0] + rot[i][1] * point[1] + rot[i][2] * point[2];
    }
    dst
}

/// `FloorDivMod(numer, denom, *quotient, *rem)`.
///
/// Returns `(quotient, remainder)` with floor-based semantics. The C calls
/// `Sys_Error` when `denom <= 0.0`; to stay total we return `(0, 0)` instead.
pub fn floor_div_mod(numer: f64, denom: f64) -> (i32, i32) {
    if denom <= 0.0 {
        // C: Sys_Error("FloorDivMod: bad denominator ...") — we stay total.
        return (0, 0);
    }

    let (q, r): (i32, i32);
    if numer >= 0.0 {
        let x = (numer / denom).floor();
        q = x as i32;
        r = (numer - (x * denom)).floor() as i32;
    } else {
        // Perform operations with positive values, then fix mod to be floored.
        let x = (-numer / denom).floor();
        let mut qq = -(x as i32);
        let mut rr = (-numer - (x * denom)).floor() as i32;
        if rr != 0 {
            qq -= 1;
            rr = denom as i32 - rr;
        }
        q = qq;
        r = rr;
    }

    (q, r)
}

/// `GreatestCommonDivisor(i1, i2)`. Faithful recursive Euclid as in C.
pub fn gcd(a: i32, b: i32) -> i32 {
    if a > b {
        if b == 0 {
            a
        } else {
            gcd(b, a % b)
        }
    } else if a == 0 {
        b
    } else {
        gcd(a, b % a)
    }
}

/// `Q_log2(val)`: integer log2 via `while (val >>= 1) answer++`.
///
/// The C uses arithmetic right shift on a signed `int`; we mirror that.
pub fn q_log2(v: i32) -> i32 {
    let mut val = v;
    let mut answer = 0;
    loop {
        val >>= 1; // arithmetic shift, matching signed int >>=
        if val == 0 {
            break;
        }
        answer += 1;
    }
    answer
}

/// `Invert24To16(val)`: inverts an 8.24 value to a 16.16 value.
///
/// C: `if (val < 256) return 0xFFFFFFFF;` (the comparison is on signed `int`)
/// `else return (fixed16_t)((double)0x10000 * (double)0x1000000 / (double)val + 0.5);`
///
/// The contract types this `u32 -> u32`. We interpret the input bit pattern as
/// the C `int` (so the `< 256` test is signed) and compute the conversion in
/// `f64`, returning the result's bit pattern as `u32`.
pub fn invert_24_to_16(val: u32) -> u32 {
    let signed = val as i32;
    if signed < 256 {
        return 0xFFFF_FFFF;
    }
    // (double)0x10000 * (double)0x1000000 / (double)val + 0.5, then (fixed16_t).
    let result = (f64::from(0x10000u32) * f64::from(0x1000000u32) / f64::from(signed) + 0.5) as i32;
    result as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-5;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() <= EPS
    }

    fn approx_vec(a: Vec3, b: Vec3) -> bool {
        approx(a[0], b[0]) && approx(a[1], b[1]) && approx(a[2], b[2])
    }

    #[test]
    fn dot_known() {
        // (1,2,3).(4,-5,6) = 4 -10 +18 = 12
        assert_eq!(dot([1.0, 2.0, 3.0], [4.0, -5.0, 6.0]), 12.0);
        assert_eq!(dot([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]), 0.0);
    }

    #[test]
    fn cross_known() {
        // x cross y = z
        assert_eq!(cross([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]), [0.0, 0.0, 1.0]);
        // y cross z = x
        assert_eq!(cross([0.0, 1.0, 0.0], [0.0, 0.0, 1.0]), [1.0, 0.0, 0.0]);
        // (1,2,3) x (4,5,6) = (-3,6,-3)
        assert_eq!(cross([1.0, 2.0, 3.0], [4.0, 5.0, 6.0]), [-3.0, 6.0, -3.0]);
    }

    #[test]
    fn add_sub_scale_inverse() {
        assert_eq!(add([1.0, 2.0, 3.0], [4.0, 5.0, 6.0]), [5.0, 7.0, 9.0]);
        assert_eq!(sub([4.0, 5.0, 6.0], [1.0, 2.0, 3.0]), [3.0, 3.0, 3.0]);
        assert_eq!(scale([1.0, 2.0, 3.0], 2.0), [2.0, 4.0, 6.0]);
        assert_eq!(inverse([1.0, -2.0, 3.0]), [-1.0, 2.0, -3.0]);
    }

    #[test]
    fn mul_add_known() {
        // a + 2*b
        assert_eq!(mul_add([1.0, 1.0, 1.0], 2.0, [1.0, 2.0, 3.0]), [3.0, 5.0, 7.0]);
        assert_eq!(vector_ma([0.0, 0.0, 0.0], 3.0, [1.0, 1.0, 1.0]), [3.0, 3.0, 3.0]);
    }

    #[test]
    fn compare_known() {
        assert!(compare([1.0, 2.0, 3.0], [1.0, 2.0, 3.0]));
        assert!(!compare([1.0, 2.0, 3.0], [1.0, 2.0, 3.5]));
    }

    #[test]
    fn length_and_normalize() {
        // (3,4,0) -> length 5, unit (0.6, 0.8, 0)
        assert!(approx(length([3.0, 4.0, 0.0]), 5.0));
        let (unit, len) = normalize([3.0, 4.0, 0.0]);
        assert!(approx(len, 5.0));
        assert!(approx_vec(unit, [0.6, 0.8, 0.0]));
    }

    #[test]
    fn normalize_zero_returns_self_and_zero() {
        let (unit, len) = normalize([0.0, 0.0, 0.0]);
        assert_eq!(len, 0.0);
        assert_eq!(unit, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn anglemod_known() {
        // anglemod(360.0) ~= 0
        assert!(approx(anglemod(360.0), 0.0));
        // anglemod(-90.0) ~= 270
        assert!(approx(anglemod(-90.0), 270.0));
        // anglemod(90.0) ~= 90
        assert!(approx(anglemod(90.0), 90.0));
    }

    #[test]
    fn angle_vectors_zero() {
        let (forward, right, up) = angle_vectors([0.0, 0.0, 0.0]);
        assert!(approx_vec(forward, [1.0, 0.0, 0.0]));
        assert!(approx_vec(right, [0.0, -1.0, 0.0]));
        assert!(approx_vec(up, [0.0, 0.0, 1.0]));
    }

    #[test]
    fn angle_vectors_yaw_90() {
        // Yaw 90 deg: forward should rotate toward +y.
        let (forward, _right, _up) = angle_vectors([0.0, 90.0, 0.0]);
        assert!(approx_vec(forward, [0.0, 1.0, 0.0]));
    }

    fn identity3() -> [[f32; 3]; 3] {
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
    }

    #[test]
    fn concat_rotations_identity() {
        let i = identity3();
        assert_eq!(concat_rotations(&i, &i), i);
    }

    #[test]
    fn concat_rotations_product() {
        let a = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]];
        let i = identity3();
        // a * I == a
        assert_eq!(concat_rotations(&a, &i), a);
        // I * a == a
        assert_eq!(concat_rotations(&i, &a), a);
    }

    #[test]
    fn concat_transforms_identity_translation() {
        // Identity rotation with a translation in the 4th column.
        let i = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ];
        let t = [
            [1.0, 0.0, 0.0, 10.0],
            [0.0, 1.0, 0.0, 20.0],
            [0.0, 0.0, 1.0, 30.0],
        ];
        // I * T should equal T (rotation identity, translation preserved).
        assert_eq!(concat_transforms(&i, &t), t);
        // T * I: translation comes from in1's 4th column added.
        let out = concat_transforms(&t, &i);
        assert_eq!(out[0][3], 10.0);
        assert_eq!(out[1][3], 20.0);
        assert_eq!(out[2][3], 30.0);
    }

    #[test]
    fn plane_new_axial_and_signbits() {
        // +X axial plane: ptype 0, no negative components -> signbits 0.
        let px = Plane::new([1.0, 0.0, 0.0], 5.0);
        assert_eq!(px.ptype, 0);
        assert_eq!(px.signbits, 0);

        // -Y axial: ptype 1, normal[1] < 0 -> bit 1 set -> signbits 2.
        let ny = Plane::new([0.0, -1.0, 0.0], -3.0);
        assert_eq!(ny.ptype, 1);
        assert_eq!(ny.signbits, 0b010);

        // -Z axial: ptype 2, bit 2 set -> signbits 4.
        let nz = Plane::new([0.0, 0.0, -1.0], 0.0);
        assert_eq!(nz.ptype, 2);
        assert_eq!(nz.signbits, 0b100);

        // Non-axial: ptype 3 (PLANE_ANYX); all negative -> signbits 7.
        let diag = Plane::new([-0.577, -0.577, -0.577], 1.0);
        assert_eq!(diag.ptype, 3);
        assert_eq!(diag.signbits, 0b111);
    }

    #[test]
    fn box_on_plane_side_both_sides() {
        // +X plane at x=5, box straddling x=5 -> both sides (3).
        let p = Plane::new([1.0, 0.0, 0.0], 5.0);
        let mins = [0.0, 0.0, 0.0];
        let maxs = [10.0, 10.0, 10.0];
        assert_eq!(box_on_plane_side(mins, maxs, &p), 3);
    }

    #[test]
    fn box_on_plane_side_one_side() {
        let p = Plane::new([1.0, 0.0, 0.0], 5.0);
        // Box entirely on the >= side (x in [6,10]) -> side 1.
        assert_eq!(box_on_plane_side([6.0, 0.0, 0.0], [10.0, 10.0, 10.0], &p), 1);
        // Box entirely on the < side (x in [0,4]) -> side 2.
        assert_eq!(box_on_plane_side([0.0, 0.0, 0.0], [4.0, 10.0, 10.0], &p), 2);
    }

    #[test]
    fn box_on_plane_side_diagonal() {
        // Non-axial plane: exercises a non-zero signbits switch arm.
        // normal points into (+,+,+); box straddles the plane.
        let p = Plane::new([0.5, 0.5, 0.5], 5.0);
        // signbits 0 (no negatives).
        assert_eq!(p.signbits, 0);
        let s = box_on_plane_side([0.0, 0.0, 0.0], [10.0, 10.0, 10.0], &p);
        // maxs dot = 15 >= 5 -> bit1; mins dot = 0 < 5 -> bit2; => 3.
        assert_eq!(s, 3);
    }

    #[test]
    fn box_on_plane_side_negative_normal() {
        // Negative-X normal exercises signbits = 1 arm.
        let p = Plane::new([-1.0, 0.0, 0.0], -5.0);
        assert_eq!(p.signbits, 1);
        // For normal (-1,0,0), dist1 uses emins[0]=-... check straddle.
        let s = box_on_plane_side([0.0, 0.0, 0.0], [10.0, 10.0, 10.0], &p);
        // signbits 1: dist1 = -mins[0]= 0; dist2 = -maxs[0] = -10.
        // dist1(0) >= dist(-5) -> bit1; dist2(-10) < -5 -> bit2 => 3.
        assert_eq!(s, 3);
    }

    #[test]
    fn box_on_plane_side_fallback_total() {
        // Construct a plane with out-of-range signbits to hit the fallback.
        let mut p = Plane::new([1.0, 0.0, 0.0], 5.0);
        p.signbits = 99; // not 0..=7
        // Should not panic; general corner formula gives the straddle answer.
        let s = box_on_plane_side([0.0, 0.0, 0.0], [10.0, 10.0, 10.0], &p);
        assert_eq!(s, 3);
    }

    #[test]
    fn project_and_perpendicular() {
        // Projecting a point onto the plane through origin with z-normal drops z.
        let dst = project_point_on_plane([1.0, 2.0, 3.0], [0.0, 0.0, 1.0]);
        assert!(approx_vec(dst, [1.0, 2.0, 0.0]));

        // Perpendicular of +Z axis must be unit-length and orthogonal to +Z.
        let perp = perpendicular_vector([0.0, 0.0, 1.0]);
        assert!(approx(length(perp), 1.0));
        assert!(approx(dot(perp, [0.0, 0.0, 1.0]), 0.0));
    }

    #[test]
    fn rotate_point_around_vector_full_turn() {
        // Rotating any point about an axis by 360 degrees returns it (approx).
        let dir = [0.0, 0.0, 1.0];
        let point = [1.0, 0.0, 0.0];
        let r = rotate_point_around_vector(dir, point, 360.0);
        assert!(approx_vec(r, point));
    }

    #[test]
    fn rotate_point_around_vector_axis_fixed() {
        // A point on the rotation axis stays (approximately) fixed.
        let dir = [0.0, 0.0, 1.0];
        let point = [0.0, 0.0, 5.0];
        let r = rotate_point_around_vector(dir, point, 90.0);
        assert!(approx_vec(r, point));
    }

    #[test]
    fn floor_div_mod_positive() {
        // 7 / 3 -> q=2, r=1
        assert_eq!(floor_div_mod(7.0, 3.0), (2, 1));
        // 6 / 3 -> q=2, r=0
        assert_eq!(floor_div_mod(6.0, 3.0), (2, 0));
    }

    #[test]
    fn floor_div_mod_negative() {
        // -7 / 3 floor-based -> q=-3, r=2
        assert_eq!(floor_div_mod(-7.0, 3.0), (-3, 2));
        // -6 / 3 -> q=-2, r=0
        assert_eq!(floor_div_mod(-6.0, 3.0), (-2, 0));
    }

    #[test]
    fn floor_div_mod_bad_denom() {
        assert_eq!(floor_div_mod(10.0, 0.0), (0, 0));
        assert_eq!(floor_div_mod(10.0, -2.0), (0, 0));
    }

    #[test]
    fn gcd_known() {
        assert_eq!(gcd(12, 18), 6);
        assert_eq!(gcd(18, 12), 6);
        assert_eq!(gcd(7, 0), 7);
        assert_eq!(gcd(0, 7), 7);
        assert_eq!(gcd(17, 5), 1);
    }

    #[test]
    fn q_log2_known() {
        assert_eq!(q_log2(1), 0);
        assert_eq!(q_log2(2), 1);
        assert_eq!(q_log2(8), 3);
        assert_eq!(q_log2(255), 7);
        assert_eq!(q_log2(256), 8);
        // val=0 -> first shift yields 0 -> answer 0
        assert_eq!(q_log2(0), 0);
    }

    #[test]
    fn invert_24_to_16_known() {
        // val < 256 -> 0xFFFFFFFF
        assert_eq!(invert_24_to_16(0), 0xFFFF_FFFF);
        assert_eq!(invert_24_to_16(255), 0xFFFF_FFFF);
        // val = 0x1000000 (1.0 in 8.24) -> 0x10000 (1.0 in 16.16)
        assert_eq!(invert_24_to_16(0x0100_0000), 0x0001_0000);
        // val = 0x2000000 (2.0 in 8.24) -> 0x8000 (0.5 in 16.16)
        assert_eq!(invert_24_to_16(0x0200_0000), 0x0000_8000);
    }
}
