//! Bounded exact-integer proof that Form BBoxes cannot crop an image.
//! Arbitrary path clips and rounded/real transforms never enter this proof.
use craft_pdf::syntax::object::{Array, Dict, Number};

pub(crate) const IDENTITY: [i64; 6] = [1, 0, 0, 1, 0, 0];
const LIMIT: i64 = 1 << 20;

pub(crate) fn array<const N: usize>(dict: &Dict<'_>, key: &[u8]) -> Option<[Number; N]> {
    let array = dict.get::<Array<'_>>(key)?;
    let mut raw = array.data();
    let mut out = [Number::ZERO; N];
    for target in &mut out {
        raw = trivia(raw);
        let end = raw.iter().position(|b| whitespace(*b) || *b == b'%').unwrap_or(raw.len());
        let token = raw.get(..end)?;
        // Direct integral array entries only. A resolving iterator can stop at
        // an unavailable reference, hiding a trailing item after the required N.
        if token.contains(&b'.') {
            return None;
        }
        *target = Number::from_i32(integral_token(token)? as i32);
        raw = raw.get(end..)?;
    }
    trivia(raw).is_empty().then_some(out)
}

pub(crate) fn integers<const N: usize>(numbers: [Number; N]) -> Option<[i64; N]> {
    let mut out = [0; N];
    for (target, number) in out.iter_mut().zip(numbers) {
        let n = number.as_i64();
        // Number equality preserves the integer/real distinction. Do not infer
        // exactness from a rounded real which happens to have no fractional part.
        if !(-LIMIT..=LIMIT).contains(&n) || number != Number::from_i32(n as i32) {
            return None;
        }
        *target = n;
    }
    Some(out)
}

pub(crate) fn concat(left: [i64; 6], right: [i64; 6]) -> Option<[i64; 6]> {
    let [a, b, c, d, e, f] = left;
    let [g, h, i, j, k, l] = right;
    // Both inputs are bounded by 2^20. Each integer product and sum is exact
    // in i64 and in the corresponding f64 placement calculation (< 2^42).
    let result = [a * g + c * h, b * g + d * h, a * i + c * j, b * i + d * j, a * k + c * l + e, b * k + d * l + f];
    result.iter().all(|v| (-LIMIT..=LIMIT).contains(v)).then_some(result)
}

/// Exporters write exact integral values as `36.000000000000`. Retain this
/// evidence from lexical operands, never by testing the rounded parser result.
pub(crate) fn content_matrix(mut raw: &[u8], numbers: [Number; 6]) -> Option<[i64; 6]> {
    let mut values = [0; 6];
    for target in &mut values {
        raw = trivia(raw);
        let end = raw.iter().position(|b| whitespace(*b) || *b == b'%').unwrap_or(raw.len());
        *target = integral_token(raw.get(..end)?)?;
        raw = raw.get(end..)?;
    }
    raw = trivia(raw);
    let rest = raw.strip_prefix(b"cm")?;
    if !trivia(rest).is_empty() || values.iter().zip(numbers).any(|(&n, parsed)| n as f64 != parsed.as_f64()) {
        return None;
    }
    Some(values)
}
fn whitespace(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}
fn trivia(mut raw: &[u8]) -> &[u8] {
    loop {
        let start = raw.iter().position(|b| !whitespace(*b)).unwrap_or(raw.len());
        raw = raw.get(start..).unwrap_or_default();
        if raw.first() == Some(&b'%') {
            let end = raw.iter().position(|b| matches!(b, b'\r' | b'\n')).unwrap_or(raw.len());
            raw = raw.get(end..).unwrap_or_default();
        } else {
            return raw;
        }
    }
}
fn integral_token(mut token: &[u8]) -> Option<i64> {
    let negative = token.first() == Some(&b'-');
    if matches!(token.first(), Some(b'+' | b'-')) {
        token = token.get(1..)?;
    }
    let mut value = 0i64;
    let (mut point, mut digit) = (false, false);
    for &b in token {
        if b == b'.' && !point {
            point = true;
        } else if b.is_ascii_digit() {
            digit = true;
            if point {
                if b != b'0' {
                    return None;
                }
            } else {
                value = value.checked_mul(10)?.checked_add(i64::from(b - b'0'))?;
                if value > LIMIT {
                    return None;
                }
            }
        } else {
            return None;
        }
    }
    digit.then_some(if negative { -value } else { value })
}

pub(crate) fn bbox(matrix: [i64; 6], bounds: [Number; 4]) -> Option<[i64; 4]> {
    let [a, b, c, d, e, f] = matrix;
    let [x0, y0, x1, y1] = integers(bounds)?;
    if b != 0 || c != 0 || a == 0 || d == 0 || x0 >= x1 || y0 >= y1 {
        return None;
    }
    let (u0, u1, v0, v1) = (a * x0 + e, a * x1 + e, d * y0 + f, d * y1 + f);
    Some([u0.min(u1), v0.min(v1), u0.max(u1), v0.max(v1)])
}

pub(crate) fn intersect(a: [i64; 4], b: [i64; 4]) -> Option<[i64; 4]> {
    let result = [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])];
    (result[0] < result[2] && result[1] < result[3]).then_some(result)
}

pub(crate) fn contains(bounds: [i64; 4], matrix: [i64; 6]) -> bool {
    let [a, b, c, d, e, f] = matrix;
    if a * d == b * c {
        return false;
    }
    // A parallelogram lies inside this convex rectangle iff all four corners
    // do. Strict interior excludes boundary-touch and needs no inferred tolerance.
    [(e, f), (a + e, b + f), (c + e, d + f), (a + c + e, b + d + f)]
        .iter()
        .all(|&(x, y)| bounds[0] < x && x < bounds[2] && bounds[1] < y && y < bounds[3])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_integer_clip_proof_refuses_rounding_and_bounds_arithmetic() {
        assert_eq!(integers([Number::from_f32(1.0)]), None);
        assert_eq!(integers([Number::from_i32(1_048_577)]), None);
        let max = [1_048_576, 0, 0, 1_048_576, 1_048_576, 1_048_576];
        assert!(concat(max, max).is_none());
        assert_eq!(concat(IDENTITY, [1, 0, 0, -1, 3, 5]), Some([1, 0, 0, -1, 3, 5]));
        assert!(!contains([0, 0, 10, 10], [0, 0, 0, 0, 5, 5]));
        assert!(!contains([0, 0, 10, 10], [8, 0, 0, 8, 2, 2]));
        assert!(contains([0, 0, 10, 10], [-8, 0, 0, 8, 9, 1]));
        assert_eq!(intersect([0, 0, 5, 5], [5, 0, 10, 5]), None);
        let matrix = [36, 0, 0, -36, 18, 126].map(Number::from_i32);
        assert_eq!(content_matrix(b"36.000 0 0 -36.000 18 126 cm", matrix), Some([36, 0, 0, -36, 18, 126]));
        assert!(content_matrix(b"36.000000000000000000001 0 0 -36 18 126 cm", matrix).is_none());
        assert!(content_matrix(b"36 0 0 -36 18 126 cm junk", matrix).is_none());
        assert_eq!(content_matrix(b"% comment\n36 0 % another\n0 -36 18 126 cm", matrix), Some([36, 0, 0, -36, 18, 126]));
    }
}
