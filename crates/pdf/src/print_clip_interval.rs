//! Conservative interval proof for non-cropping decimal Form bounds.
//! The retained rectangle is an inner subset, never the exact BBox intersection.
use craft_pdf::syntax::object::{Array, Dict, Number};
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Interval {
    lo: f64,
    hi: f64,
}
const LIMIT: f64 = 1_048_576.0;
impl Interval {
    const ZERO: Self = Self { lo: 0.0, hi: 0.0 };
    const ONE: Self = Self { lo: 1.0, hi: 1.0 };
    fn exact(n: f64) -> Option<Self> {
        (n.is_finite() && n.abs() <= LIMIT).then_some(Self { lo: n, hi: n })
    }
    fn bounded(lo: f64, hi: f64) -> Option<Self> {
        (lo.is_finite() && hi.is_finite() && lo <= hi && lo >= -LIMIT && hi <= LIMIT).then_some(Self { lo, hi })
    }
    fn add(self, rhs: Self) -> Option<Self> {
        if self == Self::ZERO {
            return Some(rhs);
        }
        if rhs == Self::ZERO {
            return Some(self);
        }
        Self::bounded((self.lo + rhs.lo).next_down(), (self.hi + rhs.hi).next_up())
    }
    fn neg(self) -> Self {
        Self { lo: -self.hi, hi: -self.lo }
    }
    fn mul(self, rhs: Self) -> Option<Self> {
        if self == Self::ZERO || rhs == Self::ZERO {
            return Some(Self::ZERO);
        }
        if self == Self::ONE {
            return Some(rhs);
        }
        if rhs == Self::ONE {
            return Some(self);
        }
        let values = [self.lo * rhs.lo, self.lo * rhs.hi, self.hi * rhs.lo, self.hi * rhs.hi];
        if values.iter().any(|x| !x.is_finite()) {
            return None;
        }
        let lo = values.into_iter().fold(f64::INFINITY, f64::min).next_down();
        let hi = values.into_iter().fold(f64::NEG_INFINITY, f64::max).next_up();
        Self::bounded(lo, hi)
    }
    fn contains(self, n: f64) -> bool {
        n.is_finite() && self.lo <= n && n <= self.hi
    }
}
fn decimal(mut token: &[u8], actual: f64) -> Option<Interval> {
    if token.len() > 32 {
        return None;
    }
    let negative = token.first() == Some(&b'-');
    if matches!(token.first(), Some(b'+' | b'-')) {
        token = token.get(1..)?;
    }
    let (mut numerator, mut scale, mut point, mut digits) = (0u64, 0u32, false, false);
    for &b in token {
        if b == b'.' && !point {
            point = true;
            continue;
        }
        if !b.is_ascii_digit() {
            return None;
        }
        digits = true;
        numerator = numerator.checked_mul(10)?.checked_add(u64::from(b - b'0'))?;
        if numerator > ((1u64 << 53) - 1) {
            return None;
        }
        if point {
            scale += 1;
            if scale > 15 {
                return None;
            }
        }
    }
    if !digits {
        return None;
    }
    let denominator = 10u64.pow(scale);
    let sign = if negative { -1.0 } else { 1.0 };
    let interval = if numerator % denominator == 0 {
        Interval::exact(sign * (numerator / denominator) as f64)?
    } else {
        // numerator and denominator are exact f64 integers; enclose the one rounded division.
        let value = sign * (numerator as f64 / denominator as f64);
        Interval::bounded(value.next_down(), value.next_up())?
    };
    interval.contains(actual).then_some(interval)
}
pub(crate) type Matrix = [Interval; 6];
pub(crate) const IDENTITY: Matrix = [Interval::ONE, Interval::ZERO, Interval::ZERO, Interval::ONE, Interval::ZERO, Interval::ZERO];
pub(crate) fn concat(left: Matrix, right: Matrix) -> Option<Matrix> {
    let [a, b, c, d, e, f] = left;
    let [g, h, i, j, k, l] = right;
    Some([
        a.mul(g)?.add(c.mul(h)?)?,
        b.mul(g)?.add(d.mul(h)?)?,
        a.mul(i)?.add(c.mul(j)?)?,
        b.mul(i)?.add(d.mul(j)?)?,
        a.mul(k)?.add(c.mul(l)?)?.add(e)?,
        b.mul(k)?.add(d.mul(l)?)?.add(f)?,
    ])
}
pub(crate) fn interior(matrix: Matrix, bounds: [Interval; 4]) -> Option<[i64; 4]> {
    let [a, b, c, d, e, f] = matrix;
    let [x0, y0, x1, y1] = bounds;
    if b != Interval::ZERO || c != Interval::ZERO || a.contains(0.0) || d.contains(0.0) || x0.hi >= x1.lo || y0.hi >= y1.lo {
        return None;
    }
    let (u0, u1, v0, v1) = (a.mul(x0)?.add(e)?, a.mul(x1)?.add(e)?, d.mul(y0)?.add(f)?, d.mul(y1)?.add(f)?);
    let (left, right) = if a.lo > 0.0 { (u0, u1) } else { (u1, u0) };
    let (bottom, top) = if d.lo > 0.0 { (v0, v1) } else { (v1, v0) };
    let rect = [left.hi.ceil() as i64, bottom.hi.ceil() as i64, right.lo.floor() as i64, top.lo.floor() as i64];
    (rect[0] < rect[2] && rect[1] < rect[3]).then_some(rect)
}
pub(crate) fn contains(bounds: [i64; 4], matrix: Matrix) -> Option<bool> {
    let [a, b, c, d, e, f] = matrix;
    if a.mul(d)?.add(b.mul(c)?.neg())?.contains(0.0) {
        return Some(false);
    }
    let corners = [(e, f), (a.add(e)?, b.add(f)?), (c.add(e)?, d.add(f)?), (a.add(c)?.add(e)?, b.add(d)?.add(f)?)];
    Some(corners.iter().all(|&(x, y)| (bounds[0] as f64) < x.lo && x.hi < bounds[2] as f64 && (bounds[1] as f64) < y.lo && y.hi < bounds[3] as f64))
}
pub(crate) fn agrees(matrix: Matrix, actual: [f64; 6]) -> bool {
    matrix.iter().zip(actual).all(|(bound, value)| bound.contains(value))
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
fn values<const N: usize>(mut raw: &[u8], actual: [f64; N]) -> Option<([Interval; N], &[u8])> {
    let mut result = [Interval::ZERO; N];
    for (target, value) in result.iter_mut().zip(actual) {
        raw = trivia(raw);
        let end = raw.iter().position(|b| whitespace(*b) || *b == b'%').unwrap_or(raw.len());
        *target = decimal(raw.get(..end)?, value)?;
        raw = raw.get(end..)?;
    }
    Some((result, trivia(raw)))
}
pub(crate) fn array<const N: usize>(dict: &Dict<'_>, key: &[u8]) -> Option<[Interval; N]> {
    let array = dict.get::<Array<'_>>(key)?;
    let actual = dict.get::<[f64; N]>(key)?;
    let (result, tail) = values(array.data(), actual)?;
    tail.is_empty().then_some(result)
}
pub(crate) fn content_matrix(raw: &[u8], actual: [Number; 6]) -> Option<Matrix> {
    let (result, tail) = values(raw, actual.map(|n| n.as_f64()))?;
    trivia(tail.strip_prefix(b"cm")?).is_empty().then_some(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fractional_forms_retain_only_a_strict_integer_interior() {
        let n = |x: f64| Interval::exact(x).unwrap();
        let bounds = [n(-0.25), n(-0.25), n(10.25), n(10.25)];
        assert_eq!(interior(IDENTITY, bounds), Some([0, 0, 10, 10]));
        let image = [n(8.5), n(0.0), n(0.0), n(8.5), n(0.25), n(0.25)];
        assert_eq!(contains([0, 0, 10, 10], image), Some(true));
        let touching = [n(10.0), n(0.0), n(0.0), n(10.0), n(0.0), n(0.0)];
        assert_eq!(contains([0, 0, 10, 10], touching), Some(false));
        assert_eq!(concat(IDENTITY, image), Some(image));
        let sheared = [n(1.0), n(0.125), n(0.0), n(1.0), n(0.0), n(0.0)];
        assert_eq!(interior(sheared, bounds), None);
    }
    #[test]
    fn strict_decimals_enclose_values_and_reject_ambiguity() {
        for s in ["0.1", "-.25", "+10.125", ".000000000000001", "1048575.5"] {
            let v = s.parse::<f64>().unwrap();
            assert!(decimal(s.as_bytes(), v).unwrap().contains(v));
        }
        for s in [".", "+", "1e2", "1.2-3", "1.0000000000000001", "9007199254740992", "1048577", "NaN"] {
            assert!(decimal(s.as_bytes(), 0.0).is_none(), "{s}");
        }
        assert_eq!(decimal(b"-0.00", -0.0), Some(Interval::ZERO));
        assert_eq!(decimal(b"1.000", 1.0), Some(Interval::ONE));
        assert!(decimal(b"0.1", 0.11).is_none());
    }
    #[test]
    fn arithmetic_keeps_identity_and_encloses_cancellation() {
        let a = decimal(b"0.1", 0.1).unwrap();
        let b = decimal(b"0.2", 0.2).unwrap();
        assert_eq!(a.mul(Interval::ZERO), Some(Interval::ZERO));
        assert_eq!(a.mul(Interval::ONE), Some(a));
        assert_eq!(a.add(Interval::ZERO), Some(a));
        assert!(a.add(b).unwrap().contains(0.3));
        assert!(a.add(a.neg()).unwrap().contains(0.0));
        assert!(a.mul(b).unwrap().contains(0.02));
        assert!(Interval::exact(LIMIT).unwrap().add(Interval::ONE).is_none());
    }
    // Compare IEEE endpoint rationals with exact decimal rationals using integers,
    // independently of floating division and the interval implementation.
    fn rational(value: f64) -> (i128, i128) {
        let bits = value.to_bits();
        let exponent = ((bits >> 52) & 0x7ff) as i32 - 1023 - 52;
        let mantissa = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
        let sign = if bits >> 63 == 0 { 1 } else { -1 };
        if exponent >= 0 { (sign * (i128::from(mantissa) << exponent), 1) } else { (sign * i128::from(mantissa), 1i128 << (-exponent)) }
    }
    fn encloses_exact(interval: Interval, numerator: i128, denominator: i128) {
        let (lo_n, lo_d) = rational(interval.lo);
        let (hi_n, hi_d) = rational(interval.hi);
        assert!(lo_n * denominator <= numerator * lo_d && numerator * hi_d <= hi_n * denominator, "{interval:?}: {numerator}/{denominator}");
    }
    #[test]
    fn decimal_and_arithmetic_bounds_enclose_independent_integer_rationals() {
        use craft_pdf::syntax::reader::{Reader, ReaderExt};
        for scale in 1..=6 {
            let denominator = 10i128.pow(scale);
            for numerator in (1..1000i128).step_by(7) {
                for sign in [-1i128, 1] {
                    let token = format!(
                        "{}{}.{:0width$}",
                        if sign < 0 { "-" } else { "" },
                        numerator / denominator,
                        numerator % denominator,
                        width = scale as usize
                    );
                    let parsed = Reader::new(token.as_bytes()).read_without_context::<Number>().unwrap().as_f64();
                    let bound = decimal(token.as_bytes(), parsed).unwrap();
                    encloses_exact(bound, sign * numerator, denominator);
                    let other = decimal(b"0.125", 0.125).unwrap();
                    encloses_exact(bound.mul(other).unwrap(), sign * numerator, denominator * 8);
                    // Avoid exact zero in the reference float decomposition.
                    if sign * numerator * 8 + denominator != 0 {
                        encloses_exact(bound.add(other).unwrap(), sign * numerator * 8 + denominator, denominator * 8);
                    }
                }
            }
        }
    }
    #[test]
    fn untrusted_lexical_operands_are_bounded_and_never_promote_tolerant_numbers() {
        use craft_pdf::syntax::reader::{Reader, ReaderExt};
        for s in ["+", "-", ".", "+.", "-.", "1.2-3", ".0000000000000001", "1.00000000000000001"] {
            let actual = Reader::new(s.as_bytes()).read_without_context::<Number>().map(|n| n.as_f64()).unwrap_or(0.0);
            assert!(decimal(s.as_bytes(), actual).is_none(), "{s}");
        }
        let mut state = 0x9654ab32u32;
        for length in 0..=64 {
            for _ in 0..64 {
                let bytes: Vec<_> = (0..length)
                    .map(|_| {
                        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                        (state >> 24) as u8
                    })
                    .collect();
                if let Some(bound) = decimal(&bytes, 0.0) {
                    assert!(bound.contains(0.0));
                }
                assert!(content_matrix(&bytes, [Number::ZERO; 6]).is_none());
            }
        }
        let parsed = [Number::ONE, Number::ZERO, Number::ZERO, Number::ONE, Number::ZERO, Number::ZERO];
        assert!(content_matrix(b"1 0 0 1 0 0 cm /Extra", parsed).is_none());
        assert!(content_matrix(b"1 0 0 1 0 0 999 0 R cm", parsed).is_none());
        assert!(content_matrix(b"1 0 %comment\n0 1 -0.000 0 cm", parsed).is_some());
    }
}
