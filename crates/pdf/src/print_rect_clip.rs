//! Bounded decimal rectangle evidence. Exact fractions retain the PDF boundary;
//! outward intervals cover parsing and arithmetic. No crop edge is rounded to a pixel.
use craft_pdf::syntax::object::{Array, Dict, Number};
use std::cmp::Ordering;

const SAFE: i128 = (1i128 << 53) - 1;
const LIMIT: f64 = 1_048_576.0;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ratio {
    n: i128,
    d: i128,
}
fn gcd(mut a: i128, mut b: i128) -> i128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}
impl Ratio {
    const ZERO: Self = Self { n: 0, d: 1 };
    const ONE: Self = Self { n: 1, d: 1 };
    fn new(n: i128, d: i128) -> Option<Self> {
        if d <= 0 {
            return None;
        }
        let g = gcd(n.checked_abs()?, d);
        Some(Self { n: n / g, d: d / g })
    }
    fn decimal(mut s: &[u8]) -> Option<Self> {
        if s.is_empty() || s.len() > 32 {
            return None;
        }
        let negative = s.first() == Some(&b'-');
        if matches!(s.first(), Some(b'+' | b'-')) {
            s = s.get(1..)?;
        }
        let (mut n, mut d, mut point, mut digit, mut scale) = (0i128, 1i128, false, false, 0);
        for &b in s {
            if b == b'.' && !point {
                point = true;
                continue;
            }
            if !b.is_ascii_digit() {
                return None;
            }
            digit = true;
            n = n.checked_mul(10)?.checked_add(i128::from(b - b'0'))?;
            if n > SAFE {
                return None;
            }
            if point {
                scale += 1;
                if scale > 15 {
                    return None;
                }
                d *= 10;
            }
        }
        if !digit {
            return None;
        }
        Self::new(if negative { -n } else { n }, d)
    }
    fn add(self, b: Self) -> Option<Self> {
        let g = gcd(self.d, b.d);
        let (a_scale, b_scale) = (b.d / g, self.d / g);
        Self::new(self.n.checked_mul(a_scale)?.checked_add(b.n.checked_mul(b_scale)?)?, self.d.checked_mul(a_scale)?)
    }
    fn neg(self) -> Option<Self> {
        Self::new(self.n.checked_neg()?, self.d)
    }
    fn mul(self, b: Self) -> Option<Self> {
        let (g, h) = (gcd(self.n.checked_abs()?, b.d), gcd(b.n.checked_abs()?, self.d));
        Self::new((self.n / g).checked_mul(b.n / h)?, (self.d / h).checked_mul(b.d / g)?)
    }
    fn div(self, b: Self) -> Option<Self> {
        if b.n == 0 {
            return None;
        }
        self.mul(Self::new(if b.n < 0 { -b.d } else { b.d }, b.n.checked_abs()?)?)
    }
    fn cmp(self, b: Self) -> Option<Ordering> {
        Some(self.n.checked_mul(b.d)?.cmp(&b.n.checked_mul(self.d)?))
    }
    fn binary(value: f64) -> Option<Self> {
        if !value.is_finite() {
            return None;
        }
        if value == 0.0 {
            return Some(Self::ZERO);
        }
        let bits = value.to_bits();
        let exp = ((bits >> 52) & 2047) as i32 - 1023 - 52;
        let n = i128::from((bits & ((1u64 << 52) - 1)) | (1u64 << 52));
        let sign = if value < 0.0 { -1 } else { 1 };
        if exp >= 0 {
            Self::new(n.checked_mul(1i128.checked_shl(exp as u32)?)?.checked_mul(sign)?, 1)
        } else {
            Self::new(n * sign, 1i128.checked_shl(exp.unsigned_abs())?)
        }
    }
    fn value(self) -> Option<f64> {
        (self.n.checked_abs()? <= SAFE && self.d <= SAFE).then_some(self.n as f64 / self.d as f64)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Scalar {
    exact: Ratio,
    lo: f64,
    hi: f64,
}
impl Scalar {
    const ZERO: Self = Self { exact: Ratio::ZERO, lo: 0.0, hi: 0.0 };
    const ONE: Self = Self { exact: Ratio::ONE, lo: 1.0, hi: 1.0 };
    fn bounded(exact: Ratio, lo: f64, hi: f64) -> Option<Self> {
        let v = exact.value()?;
        (lo.is_finite() && hi.is_finite() && lo <= hi && lo >= -LIMIT && hi <= LIMIT && lo <= v && v <= hi).then_some(Self { exact, lo, hi })
    }
    fn parsed(token: &[u8], actual: f64) -> Option<Self> {
        let exact = Ratio::decimal(token)?;
        let value = exact.value()?;
        let result = if Ratio::binary(value) == Some(exact) {
            Self::bounded(exact, value, value)?
        } else {
            Self::bounded(exact, value.next_down(), value.next_up())?
        };
        (actual.is_finite() && result.lo <= actual && actual <= result.hi).then_some(result)
    }
    fn point(n: u32) -> Option<Self> {
        Self::bounded(Ratio::new(i128::from(n), 1)?, f64::from(n), f64::from(n))
    }
    fn finish(self, b: Self, exact: Ratio, lo: f64, hi: f64) -> Option<Self> {
        let value = exact.value()?;
        if self.lo == self.hi && b.lo == b.hi && Ratio::binary(value) == Some(exact) {
            Self::bounded(exact, value, value)
        } else {
            Self::bounded(exact, lo.next_down(), hi.next_up())
        }
    }
    fn add(self, b: Self) -> Option<Self> {
        self.finish(b, self.exact.add(b.exact)?, self.lo + b.lo, self.hi + b.hi)
    }
    fn neg(self) -> Option<Self> {
        Self::bounded(self.exact.neg()?, -self.hi, -self.lo)
    }
    fn sub(self, b: Self) -> Option<Self> {
        self.add(b.neg()?)
    }
    fn mul(self, b: Self) -> Option<Self> {
        let v = [self.lo * b.lo, self.lo * b.hi, self.hi * b.lo, self.hi * b.hi];
        self.finish(b, self.exact.mul(b.exact)?, v.into_iter().fold(f64::INFINITY, f64::min), v.into_iter().fold(f64::NEG_INFINITY, f64::max))
    }
    fn div(self, b: Self) -> Option<Self> {
        if b.lo <= 0.0 && b.hi >= 0.0 {
            return None;
        }
        let v = [self.lo / b.lo, self.lo / b.hi, self.hi / b.lo, self.hi / b.hi];
        self.finish(b, self.exact.div(b.exact)?, v.into_iter().fold(f64::INFINITY, f64::min), v.into_iter().fold(f64::NEG_INFINITY, f64::max))
    }
    fn min(self, b: Self) -> Option<Self> {
        let exact = if self.exact.cmp(b.exact)? == Ordering::Greater { b.exact } else { self.exact };
        Self::bounded(exact, self.lo.min(b.lo), self.hi.min(b.hi))
    }
    fn max(self, b: Self) -> Option<Self> {
        let exact = if self.exact.cmp(b.exact)? == Ordering::Less { b.exact } else { self.exact };
        Self::bounded(exact, self.lo.max(b.lo), self.hi.max(b.hi))
    }
}
pub(crate) type Matrix = [Scalar; 6];
pub(crate) type Bounds = [Scalar; 4];
pub(crate) const IDENTITY: Matrix = [Scalar::ONE, Scalar::ZERO, Scalar::ZERO, Scalar::ONE, Scalar::ZERO, Scalar::ZERO];
fn diagonal(m: Matrix) -> bool {
    m[1].exact == Ratio::ZERO && m[2].exact == Ratio::ZERO && m[1].lo == 0.0 && m[1].hi == 0.0 && m[2].lo == 0.0 && m[2].hi == 0.0
}
pub(crate) fn concat(a: Matrix, b: Matrix) -> Option<Matrix> {
    if !diagonal(a) || !diagonal(b) {
        return None;
    }
    Some([a[0].mul(b[0])?, Scalar::ZERO, Scalar::ZERO, a[3].mul(b[3])?, a[0].mul(b[4])?.add(a[4])?, a[3].mul(b[5])?.add(a[5])?])
}
pub(crate) fn agrees(m: Matrix, actual: [f64; 6]) -> bool {
    m.iter().zip(actual).all(|(x, v)| v.is_finite() && x.lo <= v && v <= x.hi)
}
pub(crate) fn bbox(m: Matrix, r: Bounds) -> Option<Bounds> {
    if !diagonal(m) || r[0].exact.cmp(r[2].exact)? != Ordering::Less || r[1].exact.cmp(r[3].exact)? != Ordering::Less {
        return None;
    }
    let (a, b, c, d) = (m[0].mul(r[0])?.add(m[4])?, m[3].mul(r[1])?.add(m[5])?, m[0].mul(r[2])?.add(m[4])?, m[3].mul(r[3])?.add(m[5])?);
    Some([a.min(c)?, b.min(d)?, a.max(c)?, b.max(d)?])
}
pub(crate) fn rect(m: Matrix, r: Bounds) -> Option<Bounds> {
    let (x1, y1) = (r[0].add(r[2])?, r[1].add(r[3])?);
    bbox(m, [r[0].min(x1)?, r[1].min(y1)?, r[0].max(x1)?, r[1].max(y1)?])
}
pub(crate) fn intersect(a: Bounds, b: Bounds) -> Option<Bounds> {
    let r = [a[0].max(b[0])?, a[1].max(b[1])?, a[2].min(b[2])?, a[3].min(b[3])?];
    (r[0].exact.cmp(r[2].exact)? == Ordering::Less && r[1].exact.cmp(r[3].exact)? == Ordering::Less).then_some(r)
}
/// Top-down pixel edges, with a separate absolute edge error bound. A rounded
/// boundary that could change which source cells are visible is not certifiable.
pub(crate) fn pixels(r: Bounds, m: Matrix, width: u32, height: u32) -> Option<crate::PrintRectCrop> {
    if !diagonal(m) || width == 0 || height == 0 || u64::from(width) * u64::from(height) > 1_048_576 {
        return None;
    }
    let (w, h) = (Scalar::point(width)?, Scalar::point(height)?);
    let x = |v: Scalar| v.sub(m[4])?.div(m[0])?.mul(w)?.max(Scalar::ZERO)?.min(w);
    let y = |v: Scalar| Scalar::ONE.sub(v.sub(m[5])?.div(m[3])?)?.mul(h)?.max(Scalar::ZERO)?.min(h);
    let (a, b, c, d) = (x(r[0])?, y(r[1])?, x(r[2])?, y(r[3])?);
    let r = [a.min(c)?, b.min(d)?, a.max(c)?, b.max(d)?];
    if r[0].exact.cmp(r[2].exact)? != Ordering::Less || r[1].exact.cmp(r[3].exact)? != Ordering::Less {
        return None;
    }
    let mut edges = [[0u64; 2]; 4];
    let mut error: f64 = 0.0;
    for (i, (target, value)) in edges.iter_mut().zip(r).enumerate() {
        let n = value.exact.value()?;
        if (i < 2 && (value.lo.floor() != value.hi.floor() || n.floor() != value.lo.floor()))
            || (i >= 2 && (value.lo.ceil() != value.hi.ceil() || n.ceil() != value.lo.ceil()))
        {
            return None;
        }
        let e = (n - value.lo).abs().max((value.hi - n).abs());
        if e > 0.000001 {
            return None;
        }
        error = error.max(e);
        *target = [u64::try_from(value.exact.n).ok()?, u64::try_from(value.exact.d).ok()?];
    }
    Some(crate::PrintRectCrop { edges, edge_error_pixels: if error == 0.0 { 0.0 } else { error.next_up() } })
}
fn whitespace(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}
fn trivia(mut raw: &[u8]) -> &[u8] {
    loop {
        let start = raw.iter().position(|b| !whitespace(*b)).unwrap_or(raw.len());
        raw = raw.get(start..).unwrap_or_default();
        if raw.first() != Some(&b'%') {
            return raw;
        }
        let end = raw.iter().position(|b| matches!(b, b'\r' | b'\n')).unwrap_or(raw.len());
        raw = raw.get(end..).unwrap_or_default();
    }
}
fn values<const N: usize>(mut raw: &[u8], actual: [f64; N]) -> Option<([Scalar; N], &[u8])> {
    let mut result = [Scalar::ZERO; N];
    for (target, value) in result.iter_mut().zip(actual) {
        raw = trivia(raw);
        let end = raw.iter().position(|b| whitespace(*b) || *b == b'%').unwrap_or(raw.len());
        *target = Scalar::parsed(raw.get(..end)?, value)?;
        raw = raw.get(end..)?;
    }
    Some((result, trivia(raw)))
}
pub(crate) fn content<const N: usize>(raw: &[u8], actual: [Number; N], operator: &[u8]) -> Option<[Scalar; N]> {
    let (result, tail) = values(raw, actual.map(|n| n.as_f64()))?;
    trivia(tail.strip_prefix(operator)?).is_empty().then_some(result)
}
pub(crate) fn array<const N: usize>(d: &Dict<'_>, key: &[u8]) -> Option<[Scalar; N]> {
    let (result, tail) = values(d.get::<Array<'_>>(key)?.data(), d.get::<[f64; N]>(key)?)?;
    tail.is_empty().then_some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decimal_edges_keep_fractions_and_bound_real_parser_error() {
        let s = |v: &str| Scalar::parsed(v.as_bytes(), v.parse().unwrap()).unwrap();
        let r = rect(IDENTITY, [s("18.125"), s("18"), s("21.875"), s("36")]).unwrap();
        let m = [s("35.5"), s("0"), s("0"), s("36"), s("18"), s("18")];
        let p = pixels(r, m, 3, 3).unwrap();
        assert_eq!(p.edges[0], [3, 284]);
        assert!(p.edge_error_pixels < 0.000001);
        assert!(Scalar::parsed(b"0.1", 0.1).is_some());
        assert!(Scalar::parsed(b"0.1", 0.10001).is_none());
        assert!(pixels(r, m, 1_048_577, 1).is_none());
    }
    #[test]
    fn arithmetic_refuses_precision_overflow_and_ambiguous_pixel_visibility() {
        for s in [b"1.0000000000000001".as_slice(), b"--1", b"1e2", b".", b"9007199254740992", b"1/2"] {
            assert!(Ratio::decimal(s).is_none());
        }
        assert!(Ratio::new(i128::MIN, 1).is_none());
        assert!(Ratio::new(1, 0).is_none());
        let a = Ratio::new(i128::MAX, 1).unwrap();
        assert!(a.add(a).is_none());
        assert!(a.mul(a).is_none());
        let s = |v: f64| Scalar::bounded(Ratio::binary(v).unwrap(), v, v).unwrap();
        let mut r = [s(1.0), s(0.0), s(2.0), s(3.0)];
        r[0].lo = 1.0f64.next_down();
        r[0].hi = 1.0f64.next_up();
        assert!(pixels(r, [s(3.0), s(0.0), s(0.0), s(3.0), s(0.0), s(0.0)], 3, 3).is_none());
    }
}
