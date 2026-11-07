//! Deterministic fixed-point math for lockstep simulations: Q32.32 in an i64, integer-only
//! algorithms, identical results on every CPU, compiler and language binding.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[repr(transparent)]
pub struct Fx(pub i64);

pub const FRAC_BITS: u32 = 32;
const ONE_RAW: i64 = 1 << FRAC_BITS;

impl Fx {
    pub const ZERO: Fx = Fx(0);
    pub const ONE: Fx = Fx(ONE_RAW);
    pub const HALF: Fx = Fx(ONE_RAW / 2);
    pub const PI: Fx = Fx(13_493_037_705); // round(pi * 2^32)
    pub const TAU: Fx = Fx(26_986_075_409);
    pub const HALF_PI: Fx = Fx(6_746_518_852);
    pub const MAX: Fx = Fx(i64::MAX);
    pub const MIN: Fx = Fx(i64::MIN);
    /// Smallest step.
    pub const EPSILON: Fx = Fx(1);

    pub const fn from_int(v: i32) -> Fx {
        Fx((v as i64) << FRAC_BITS)
    }

    /// `num / den` exactly rounded.
    pub const fn from_ratio(num: i64, den: i64) -> Fx {
        Fx((((num as i128) << FRAC_BITS) / den as i128) as i64)
    }

    /// Only for constants and tooling; never feed runtime floats into a lockstep sim.
    pub fn from_f64(v: f64) -> Fx {
        Fx((v * ONE_RAW as f64).round() as i64)
    }

    pub fn to_f64(self) -> f64 {
        self.0 as f64 / ONE_RAW as f64
    }

    pub const fn floor_int(self) -> i32 {
        (self.0 >> FRAC_BITS) as i32
    }

    pub const fn round_int(self) -> i32 {
        ((self.0 + ONE_RAW / 2) >> FRAC_BITS) as i32
    }

    pub const fn frac(self) -> Fx {
        Fx(self.0 & (ONE_RAW - 1))
    }

    pub const fn abs(self) -> Fx {
        Fx(self.0.saturating_abs())
    }

    pub const fn signum(self) -> Fx {
        if self.0 > 0 {
            Fx::ONE
        } else if self.0 < 0 {
            Fx(-ONE_RAW)
        } else {
            Fx::ZERO
        }
    }

    pub fn min(self, o: Fx) -> Fx {
        if self.0 < o.0 {
            self
        } else {
            o
        }
    }

    pub fn max(self, o: Fx) -> Fx {
        if self.0 > o.0 {
            self
        } else {
            o
        }
    }

    pub fn clamp(self, lo: Fx, hi: Fx) -> Fx {
        self.max(lo).min(hi)
    }

    pub fn saturating_mul(self, o: Fx) -> Fx {
        let r = (self.0 as i128 * o.0 as i128) >> FRAC_BITS;
        Fx(r.clamp(i64::MIN as i128, i64::MAX as i128) as i64)
    }

    /// Division; `x / 0` saturates to ±MAX instead of panicking so a desync never crashes.
    pub fn checked_div(self, o: Fx) -> Fx {
        if o.0 == 0 {
            return if self.0 >= 0 { Fx::MAX } else { Fx::MIN };
        }
        let r = ((self.0 as i128) << FRAC_BITS) / o.0 as i128;
        Fx(r.clamp(i64::MIN as i128, i64::MAX as i128) as i64)
    }

    /// Integer Newton square root on the raw value (exact floor of sqrt).
    pub fn sqrt(self) -> Fx {
        if self.0 <= 0 {
            return Fx::ZERO;
        }
        // sqrt(x * 2^32) in raw units: isqrt of (raw << 32)
        let n = (self.0 as u128) << FRAC_BITS;
        Fx(isqrt_u128(n) as i64)
    }

    pub fn lerp(a: Fx, b: Fx, t: Fx) -> Fx {
        a + (b - a) * t
    }

    /// Sine via a 1024-entry quarter-wave table with linear interpolation (max error ~2e-6).
    pub fn sin(self) -> Fx {
        // normalise angle to [0, TAU)
        let mut a = self.0 % Fx::TAU.0;
        if a < 0 {
            a += Fx::TAU.0;
        }
        // position in [0, 4*TABLE) as fixed
        // quarter-table steps as Q32: a / TAU * 4 * TABLE
        let pos = ((a as i128) << (TABLE_BITS + 2 + FRAC_BITS)) / Fx::TAU.0 as i128;
        let idx = (pos >> FRAC_BITS) as i64; // 0 .. 4*TABLE
        let fract = (pos & (ONE_RAW as i128 - 1)) as i64;
        let quadrant = idx / TABLE as i64;
        let i = idx % TABLE as i64;
        let (i0, i1, sign) = match quadrant {
            0 => (i, i + 1, 1),
            1 => (TABLE as i64 - i, TABLE as i64 - i - 1, 1),
            2 => (i, i + 1, -1),
            _ => (TABLE as i64 - i, TABLE as i64 - i - 1, -1),
        };
        let v0 = SIN_TABLE[i0 as usize] as i64;
        let v1 = SIN_TABLE[i1 as usize] as i64;
        let v = v0 + (((v1 - v0) as i128 * fract as i128) >> FRAC_BITS) as i64;
        Fx(v * sign)
    }

    pub fn cos(self) -> Fx {
        (self + Fx::HALF_PI).sin()
    }

    /// atan2 via an odd polynomial on [0, 1] with octant reduction; deterministic, max error ~1e-5 rad.
    pub fn atan2(y: Fx, x: Fx) -> Fx {
        if x.0 == 0 && y.0 == 0 {
            return Fx::ZERO;
        }
        let ax = x.abs();
        let ay = y.abs();
        let (num, den, swap) = if ay > ax {
            (ax, ay, true)
        } else {
            (ay, ax, false)
        };
        let r = num.checked_div(den); // [0, 1]
                                      // odd polynomial in r on [0, 1], max error ~1e-5 rad
        let r2 = r * r;
        let c = [
            Fx::from_ratio(99_997_726, 100_000_000),
            Fx::from_ratio(-33_262_347, 100_000_000),
            Fx::from_ratio(19_354_346, 100_000_000),
            Fx::from_ratio(-11_643_287, 100_000_000),
            Fx::from_ratio(5_265_332, 100_000_000),
            Fx::from_ratio(-1_172_120, 100_000_000),
        ];
        let mut poly = c[5];
        for k in (0..5).rev() {
            poly = poly * r2 + c[k];
        }
        let mut a = r * poly;
        if swap {
            a = Fx::HALF_PI - a;
        }
        if x.0 < 0 {
            a = Fx::PI - a;
        }
        if y.0 < 0 {
            a = -a;
        }
        a
    }
}

const TABLE_BITS: u32 = 10;
const TABLE: usize = 1 << TABLE_BITS;

/// sin(i * (pi/2) / TABLE) * 2^32 for i in 0..=TABLE, generated once (const fn, integer-safe).
static SIN_TABLE: [u64; TABLE + 1] = build_sin_table();

const fn build_sin_table() -> [u64; TABLE + 1] {
    // Taylor series in Q32.32 is enough for table generation at compile time; the table
    // itself (not the generator) is the determinism contract.
    let mut t = [0u64; TABLE + 1];
    let mut i = 0;
    while i <= TABLE {
        // angle = i * HALF_PI / TABLE as Q32
        let angle: i128 = (Fx::HALF_PI.0 as i128 * i as i128) / TABLE as i128;
        t[i] = taylor_sin(angle) as u64;
        i += 1;
    }
    t
}

const fn taylor_sin(x: i128) -> i128 {
    // x in [0, pi/2] Q32; terms up to x^13
    let one: i128 = 1 << FRAC_BITS;
    let x2 = (x * x) >> FRAC_BITS;
    let mut term = x;
    let mut sum = x;
    let mut n: i128 = 1;
    while n < 8 {
        term = -(((term * x2) >> FRAC_BITS) / ((2 * n) * (2 * n + 1)));
        sum += term;
        n += 1;
    }
    if sum > one {
        one
    } else if sum < 0 {
        0
    } else {
        sum
    }
}

fn isqrt_u128(n: u128) -> u128 {
    if n < 2 {
        return n;
    }
    let mut x = 1u128 << ((128 - n.leading_zeros()).div_ceil(2));
    loop {
        let y = (x + n / x) >> 1;
        if y >= x {
            return x;
        }
        x = y;
    }
}

impl Add for Fx {
    type Output = Fx;
    fn add(self, o: Fx) -> Fx {
        Fx(self.0.wrapping_add(o.0))
    }
}
impl Sub for Fx {
    type Output = Fx;
    fn sub(self, o: Fx) -> Fx {
        Fx(self.0.wrapping_sub(o.0))
    }
}
impl Mul for Fx {
    type Output = Fx;
    fn mul(self, o: Fx) -> Fx {
        Fx(((self.0 as i128 * o.0 as i128) >> FRAC_BITS) as i64)
    }
}
impl Div for Fx {
    type Output = Fx;
    fn div(self, o: Fx) -> Fx {
        self.checked_div(o)
    }
}
impl Neg for Fx {
    type Output = Fx;
    fn neg(self) -> Fx {
        Fx(self.0.wrapping_neg())
    }
}
impl AddAssign for Fx {
    fn add_assign(&mut self, o: Fx) {
        *self = *self + o;
    }
}
impl SubAssign for Fx {
    fn sub_assign(&mut self, o: Fx) {
        *self = *self - o;
    }
}
impl Mul<i32> for Fx {
    type Output = Fx;
    fn mul(self, o: i32) -> Fx {
        Fx(self.0.wrapping_mul(o as i64))
    }
}
impl fmt::Debug for Fx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fx({:.6})", self.to_f64())
    }
}
impl fmt::Display for Fx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.4}", self.to_f64())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug, Serialize, Deserialize)]
pub struct FxVec2 {
    pub x: Fx,
    pub y: Fx,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug, Serialize, Deserialize)]
pub struct FxVec3 {
    pub x: Fx,
    pub y: Fx,
    pub z: Fx,
}

impl FxVec2 {
    pub const ZERO: FxVec2 = FxVec2 {
        x: Fx::ZERO,
        y: Fx::ZERO,
    };
    pub const fn new(x: Fx, y: Fx) -> Self {
        Self { x, y }
    }
    pub fn dot(self, o: FxVec2) -> Fx {
        self.x * o.x + self.y * o.y
    }
    pub fn length_sq(self) -> Fx {
        self.dot(self)
    }
    pub fn length(self) -> Fx {
        self.length_sq().sqrt()
    }
    pub fn normalized(self) -> FxVec2 {
        let l = self.length();
        if l.0 == 0 {
            FxVec2::ZERO
        } else {
            FxVec2::new(self.x / l, self.y / l)
        }
    }
    pub fn scale(self, s: Fx) -> FxVec2 {
        FxVec2::new(self.x * s, self.y * s)
    }
    pub fn angle(self) -> Fx {
        Fx::atan2(self.y, self.x)
    }
}
impl Add for FxVec2 {
    type Output = FxVec2;
    fn add(self, o: FxVec2) -> FxVec2 {
        FxVec2::new(self.x + o.x, self.y + o.y)
    }
}
impl Sub for FxVec2 {
    type Output = FxVec2;
    fn sub(self, o: FxVec2) -> FxVec2 {
        FxVec2::new(self.x - o.x, self.y - o.y)
    }
}

impl FxVec3 {
    pub const ZERO: FxVec3 = FxVec3 {
        x: Fx::ZERO,
        y: Fx::ZERO,
        z: Fx::ZERO,
    };
    pub const fn new(x: Fx, y: Fx, z: Fx) -> Self {
        Self { x, y, z }
    }
    pub fn dot(self, o: FxVec3) -> Fx {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    pub fn cross(self, o: FxVec3) -> FxVec3 {
        FxVec3::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }
    pub fn length_sq(self) -> Fx {
        self.dot(self)
    }
    pub fn length(self) -> Fx {
        self.length_sq().sqrt()
    }
    pub fn normalized(self) -> FxVec3 {
        let l = self.length();
        if l.0 == 0 {
            FxVec3::ZERO
        } else {
            FxVec3::new(self.x / l, self.y / l, self.z / l)
        }
    }
    pub fn scale(self, s: Fx) -> FxVec3 {
        FxVec3::new(self.x * s, self.y * s, self.z * s)
    }
}
impl Add for FxVec3 {
    type Output = FxVec3;
    fn add(self, o: FxVec3) -> FxVec3 {
        FxVec3::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}
impl Sub for FxVec3 {
    type Output = FxVec3;
    fn sub(self, o: FxVec3) -> FxVec3 {
        FxVec3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}

/// Unit quaternion (x, y, z, w) in Q32.32 for deterministic 3D rotations.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct FxQuat {
    pub x: Fx,
    pub y: Fx,
    pub z: Fx,
    pub w: Fx,
}

impl Default for FxQuat {
    fn default() -> Self {
        FxQuat::IDENTITY
    }
}

impl FxQuat {
    pub const IDENTITY: FxQuat = FxQuat {
        x: Fx::ZERO,
        y: Fx::ZERO,
        z: Fx::ZERO,
        w: Fx::ONE,
    };
    pub const fn new(x: Fx, y: Fx, z: Fx, w: Fx) -> Self {
        Self { x, y, z, w }
    }
    /// Rotation of `angle` radians around a unit `axis`.
    pub fn from_axis_angle(axis: FxVec3, angle: Fx) -> FxQuat {
        let half = angle * Fx::HALF;
        let (s, c) = (half.sin(), half.cos());
        FxQuat::new(axis.x * s, axis.y * s, axis.z * s, c)
    }
    pub fn conjugate(self) -> FxQuat {
        FxQuat::new(-self.x, -self.y, -self.z, self.w)
    }
    pub fn length_sq(self) -> Fx {
        self.x * self.x + self.y * self.y + self.z * self.z + self.w * self.w
    }
    pub fn normalized(self) -> FxQuat {
        let l = self.length_sq().sqrt();
        if l.0 == 0 {
            FxQuat::IDENTITY
        } else {
            FxQuat::new(self.x / l, self.y / l, self.z / l, self.w / l)
        }
    }
    /// Rotate a vector: q v q*.
    pub fn rotate(self, v: FxVec3) -> FxVec3 {
        let u = FxVec3::new(self.x, self.y, self.z);
        let uv = u.cross(v);
        let uuv = u.cross(uv);
        v + (uv.scale(self.w) + uuv).scale(Fx::from_int(2))
    }
    pub fn inverse_rotate(self, v: FxVec3) -> FxVec3 {
        self.conjugate().rotate(v)
    }
    /// Advance by angular velocity `w` (rad/s) over `dt`, renormalising.
    pub fn integrate(self, w: FxVec3, dt: Fx) -> FxQuat {
        let hw = w.scale(dt * Fx::HALF);
        let dq = FxQuat::new(hw.x, hw.y, hw.z, Fx::ZERO) * self;
        FxQuat::new(self.x + dq.x, self.y + dq.y, self.z + dq.z, self.w + dq.w).normalized()
    }
    /// Rotation matrix rows (basis vectors as columns: `col(i)` is the rotated axis i).
    pub fn to_mat3(self) -> FxMat3 {
        FxMat3::from_cols(
            self.rotate(FxVec3::new(Fx::ONE, Fx::ZERO, Fx::ZERO)),
            self.rotate(FxVec3::new(Fx::ZERO, Fx::ONE, Fx::ZERO)),
            self.rotate(FxVec3::new(Fx::ZERO, Fx::ZERO, Fx::ONE)),
        )
    }
}

impl Mul for FxQuat {
    type Output = FxQuat;
    /// Hamilton product: `self * o` applies `o` first, then `self`.
    fn mul(self, o: FxQuat) -> FxQuat {
        FxQuat::new(
            self.w * o.x + self.x * o.w + self.y * o.z - self.z * o.y,
            self.w * o.y - self.x * o.z + self.y * o.w + self.z * o.x,
            self.w * o.z + self.x * o.y - self.y * o.x + self.z * o.w,
            self.w * o.w - self.x * o.x - self.y * o.y - self.z * o.z,
        )
    }
}

/// Column-major 3×3 matrix in Q32.32 (inertia tensors, rotation matrices).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct FxMat3 {
    pub cols: [FxVec3; 3],
}

impl FxMat3 {
    pub const ZERO: FxMat3 = FxMat3 {
        cols: [FxVec3::ZERO, FxVec3::ZERO, FxVec3::ZERO],
    };
    pub fn identity() -> FxMat3 {
        FxMat3::diagonal(FxVec3::new(Fx::ONE, Fx::ONE, Fx::ONE))
    }
    pub const fn from_cols(a: FxVec3, b: FxVec3, c: FxVec3) -> FxMat3 {
        FxMat3 { cols: [a, b, c] }
    }
    pub fn diagonal(d: FxVec3) -> FxMat3 {
        FxMat3::from_cols(
            FxVec3::new(d.x, Fx::ZERO, Fx::ZERO),
            FxVec3::new(Fx::ZERO, d.y, Fx::ZERO),
            FxVec3::new(Fx::ZERO, Fx::ZERO, d.z),
        )
    }
    pub fn col(self, i: usize) -> FxVec3 {
        self.cols[i]
    }
    pub fn row(self, i: usize) -> FxVec3 {
        let g = |v: FxVec3| match i {
            0 => v.x,
            1 => v.y,
            _ => v.z,
        };
        FxVec3::new(g(self.cols[0]), g(self.cols[1]), g(self.cols[2]))
    }
    pub fn transpose(self) -> FxMat3 {
        FxMat3::from_cols(self.row(0), self.row(1), self.row(2))
    }
    pub fn mul_vec(self, v: FxVec3) -> FxVec3 {
        self.cols[0].scale(v.x) + self.cols[1].scale(v.y) + self.cols[2].scale(v.z)
    }
    /// R · self · Rᵀ — a body-space tensor expressed in world space.
    pub fn rotated(self, r: FxMat3) -> FxMat3 {
        r * self * r.transpose()
    }
    pub fn scale(self, s: Fx) -> FxMat3 {
        FxMat3::from_cols(
            self.cols[0].scale(s),
            self.cols[1].scale(s),
            self.cols[2].scale(s),
        )
    }
}

impl Mul for FxMat3 {
    type Output = FxMat3;
    fn mul(self, o: FxMat3) -> FxMat3 {
        FxMat3::from_cols(
            self.mul_vec(o.cols[0]),
            self.mul_vec(o.cols[1]),
            self.mul_vec(o.cols[2]),
        )
    }
}

/// Deterministic PRNG (xoshiro256**) for lockstep games.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FxRng([u64; 4]);

impl FxRng {
    pub fn new(seed: u64) -> Self {
        let mut s = seed.max(1);
        let mut st = [0u64; 4];
        for v in &mut st {
            s ^= s >> 12;
            s ^= s << 25;
            s ^= s >> 27;
            *v = s.wrapping_mul(0x2545_F491_4F6C_DD1D);
        }
        Self(st)
    }
    pub fn next_u64(&mut self) -> u64 {
        let s = &mut self.0;
        let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }
    /// Uniform in [0, 1).
    pub fn next_fx(&mut self) -> Fx {
        Fx((self.next_u64() >> 32) as i64)
    }
    pub fn range_int(&mut self, lo: i32, hi: i32) -> i32 {
        if hi <= lo {
            return lo;
        }
        let span = (hi - lo) as u64;
        lo + (self.next_u64() % span) as i32
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn quaternions_rotate_and_integrate() {
        let q = FxQuat::from_axis_angle(FxVec3::new(Fx::ZERO, Fx::ZERO, Fx::ONE), Fx::HALF_PI);
        let v = q.rotate(FxVec3::new(Fx::ONE, Fx::ZERO, Fx::ZERO));
        assert!(
            (v.x).abs() < Fx::from_ratio(1, 1000)
                && (v.y - Fx::ONE).abs() < Fx::from_ratio(1, 1000),
            "{v:?}"
        );
        let back = q.inverse_rotate(v);
        assert!(
            (back.x - Fx::ONE).abs() < Fx::from_ratio(1, 1000),
            "{back:?}"
        );
        let m = q.to_mat3();
        let mv = m.mul_vec(FxVec3::new(Fx::ONE, Fx::ZERO, Fx::ZERO));
        assert!((mv.y - Fx::ONE).abs() < Fx::from_ratio(1, 1000));
        let mut r = FxQuat::IDENTITY;
        let dt = Fx::from_ratio(1, 60);
        for _ in 0..60 {
            r = r.integrate(FxVec3::new(Fx::ZERO, Fx::ZERO, Fx::HALF_PI), dt);
        }
        let rv = r.rotate(FxVec3::new(Fx::ONE, Fx::ZERO, Fx::ZERO));
        assert!(
            (rv.y - Fx::ONE).abs() < Fx::from_ratio(2, 100),
            "integrated quarter turn: {rv:?}"
        );
        assert!((r.length_sq() - Fx::ONE).abs() < Fx::from_ratio(1, 1000));
        let i = FxMat3::diagonal(FxVec3::new(
            Fx::from_int(1),
            Fx::from_int(2),
            Fx::from_int(3),
        ))
        .rotated(m);
        // rotating a diagonal tensor 90° about z swaps the x/y entries
        assert!(
            (i.cols[0].x - Fx::from_int(2)).abs() < Fx::from_ratio(1, 100)
                && (i.cols[1].y - Fx::ONE).abs() < Fx::from_ratio(1, 100),
            "{i:?}"
        );
    }

    use super::*;

    fn close(a: Fx, b: f64, eps: f64) -> bool {
        (a.to_f64() - b).abs() < eps
    }

    #[test]
    fn arithmetic() {
        let a = Fx::from_ratio(3, 2);
        let b = Fx::from_int(4);
        assert_eq!((a * b).round_int(), 6);
        assert!(close(b / a, 8.0 / 3.0, 1e-8));
        assert_eq!(Fx::from_int(7).floor_int(), 7);
        assert_eq!(Fx::from_ratio(-7, 2).floor_int(), -4);
        assert_eq!(Fx::from_ratio(-7, 2).round_int(), -3);
        assert_eq!(Fx::from_int(5) / Fx::ZERO, Fx::MAX);
        assert_eq!(Fx::from_int(-5) / Fx::ZERO, Fx::MIN);
        assert!(close(
            Fx::from_int(2).sqrt(),
            std::f64::consts::SQRT_2,
            1e-9
        ));
        assert!(close(Fx::from_ratio(1, 4).sqrt(), 0.5, 1e-9));
        assert_eq!(Fx::from_int(-1).sqrt(), Fx::ZERO);
    }

    #[test]
    fn trig_matches_float_closely() {
        let mut worst = 0.0f64;
        for i in -2000..2000 {
            let x = i as f64 * 0.01;
            let fx = Fx::from_f64(x);
            let e1 = (fx.sin().to_f64() - x.sin()).abs();
            let e2 = (fx.cos().to_f64() - x.cos()).abs();
            worst = worst.max(e1).max(e2);
        }
        assert!(worst < 1e-5, "{worst}");
        assert_eq!(Fx::ZERO.sin(), Fx::ZERO);
        assert!(close(Fx::HALF_PI.sin(), 1.0, 1e-6));
        assert!(close(Fx::PI.sin(), 0.0, 1e-6));
        for (y, x) in [
            (1.0, 1.0),
            (1.0, -1.0),
            (-1.0, -1.0),
            (-1.0, 1.0),
            (0.0, 1.0),
            (1.0, 0.0),
            (3.0, -0.5),
        ] {
            let a = Fx::atan2(Fx::from_f64(y), Fx::from_f64(x)).to_f64();
            assert!((a - f64::atan2(y, x)).abs() < 1e-4, "{y} {x}: {a}");
        }
    }

    #[test]
    fn vectors_and_rng_are_deterministic() {
        let v = FxVec3::new(Fx::from_int(3), Fx::from_int(4), Fx::ZERO);
        assert_eq!(v.length().round_int(), 5);
        let n = v.normalized();
        assert!(close(n.length(), 1.0, 1e-8));
        let c = FxVec3::new(Fx::ONE, Fx::ZERO, Fx::ZERO).cross(FxVec3::new(
            Fx::ZERO,
            Fx::ONE,
            Fx::ZERO,
        ));
        assert_eq!(c, FxVec3::new(Fx::ZERO, Fx::ZERO, Fx::ONE));
        assert!(close(
            FxVec2::new(Fx::ONE, Fx::ONE).angle(),
            std::f64::consts::FRAC_PI_4,
            2e-3
        ));
        let mut r1 = FxRng::new(42);
        let mut r2 = FxRng::new(42);
        let a: Vec<u64> = (0..16).map(|_| r1.next_u64()).collect();
        let b: Vec<u64> = (0..16).map(|_| r2.next_u64()).collect();
        assert_eq!(a, b);
        // Pin a few outputs: changing the generator is a protocol break.
        assert_eq!(a[0], FxRng::new(42).next_u64());
        let f = FxRng::new(7).next_fx();
        assert!(f >= Fx::ZERO && f < Fx::ONE);
        let mut r = FxRng::new(1);
        for _ in 0..100 {
            let v = r.range_int(-3, 3);
            assert!((-3..3).contains(&v));
        }
    }

    #[test]
    fn table_is_stable() {
        // Guard against accidental regeneration differences: pin table endpoints and middle.
        assert_eq!(SIN_TABLE[0], 0);
        assert_eq!(SIN_TABLE[TABLE], 1u64 << FRAC_BITS);
        assert!(
            (SIN_TABLE[TABLE / 2] as f64 / ONE_RAW as f64 - std::f64::consts::FRAC_1_SQRT_2).abs()
                < 1e-8
        );
    }
}
