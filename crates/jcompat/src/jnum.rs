//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/metrics/remote/graphite/GraphiteDataSource.kt (Java library semantics).
//! Promoted from WP 0.3b, `spikes/yaml-node/src/jnum.rs`; retains its checked arithmetic.
//! The Java number parsing and printing the binder depends on: `Double.parseDouble`, `Double.toString`,
//! `Long.parseLong`, and the slice of `BigDecimal` that `DecimalUtils.extractSecondsAndNanos` uses.

/// Character.digit(int, radix), Unicode decimal digits and fullwidth Latin.
/// Java integer parsers iterate UTF-16 chars, so supplementary digits do not count.
pub fn digit(c: u32, radix: u32) -> Option<u32> {
    if !(2..=36).contains(&radix) {
        return None;
    }
    let v = match c {
        0x30..=0x39 => c - 0x30,
        0x61..=0x7a => c - 0x61 + 10,
        0x41..=0x5a => c - 0x41 + 10,
        0xff10..=0xff19 => c - 0xff10,
        0xff21..=0xff3a => c - 0xff21 + 10,
        0xff41..=0xff5a => c - 0xff41 + 10,
        _ => {
            const ZEROES: &[u32] = &[
                0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66, 0xde6, 0xe50,
                0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90, 0x1b50, 0x1bb0, 0x1c40,
                0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0,
            ];
            c - *ZEROES
                .iter()
                .chain(SUPPLEMENTARY_ZEROES)
                .find(|&&z| c >= z && c < z + 10)?
        }
    };
    (v < radix).then_some(v)
}
const SUPPLEMENTARY_ZEROES: &[u32] = &[
    0x104a0, 0x10d30, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0, 0x11650, 0x116c0, 0x11730,
    0x118e0, 0x11950, 0x11c50, 0x11d50, 0x11da0, 0x11f50, 0x16a60, 0x16ac0, 0x16b50, 0x1d7ce, 0x1d7d8, 0x1d7e2,
    0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e950, 0x1fbf0,
];

/// Character.isDigit(int), using the reference JDK 21's supplementary Nd blocks.
/// Scanner predicates see code points; Integer.parseInt still consumes UTF-16 chars.
pub fn is_digit(c: u32) -> bool {
    digit(c, 10).is_some()
}

pub fn parse_radix(s: &str, radix: u32) -> Option<i64> {
    if !(2..=36).contains(&radix) {
        return None;
    }
    let (negative, body) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    if body.is_empty() {
        return None;
    }
    let mut v = 0i64;
    for c in body.encode_utf16() {
        let d = i64::from(digit(u32::from(c), radix)?);
        v = v.checked_mul(i64::from(radix))?;
        v = if negative { v.checked_sub(d)? } else { v.checked_add(d)? };
    }
    Some(v)
}
pub fn parse_long(s: &str) -> Option<i64> {
    parse_radix(s, 10)
}

/// `Integer.parseInt` / Kotlin `String.toIntOrNull()`: same digits as
/// `parse_long`, with the 32-bit range Java throws outside of.
pub fn parse_int(s: &str) -> Option<i32> {
    i32::try_from(parse_long(s)?).ok()
}

/// `Integer.decode`: an optional sign, then a `0x`/`0X`/`#` prefix (hex) or a
/// leading `0` (octal) selects the radix. A second sign after the prefix and
/// overflow of the signed 32-bit range throw `NumberFormatException` (None
/// here), so `0x80000000` fails while `-0x80000000` is `i32::MIN`.
// JVM evidence: WP 6.3 findings (reference JBR, Integer.decode probes).
pub fn decode_int(s: &str) -> Option<i32> {
    let (negative, rest) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let (radix, digits) = if let Some(d) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        (16, d)
    } else if let Some(d) = rest.strip_prefix('#') {
        (16, d)
    } else if rest.len() > 1 && rest.starts_with('0') {
        (8, &rest[1..])
    } else {
        (10, rest)
    };
    if digits.starts_with(['-', '+']) {
        return None;
    }
    let signed = if negative {
        format!("-{digits}")
    } else {
        digits.to_owned()
    };
    i32::try_from(parse_radix(&signed, radix)?).ok()
}

/// Double.parseDouble with the NumberFormatException message used in logs.
pub fn parse_double_with_error(s: &str) -> Result<f64, String> {
    parse_double(s).ok_or_else(|| {
        let trimmed = s.trim_matches(|c: char| c <= ' ');
        if trimmed.is_empty() {
            "empty String".to_owned()
        } else {
            format!("For input string: \"{trimmed}\"")
        }
    })
}

/// `Double.parseDouble`: trims, then accepts `[+-]` followed by `NaN`, `Infinity`, a hexadecimal float, or a decimal
/// with an optional exponent, each optionally followed by one of `fFdD`.
pub fn parse_double(s: &str) -> Option<f64> {
    let t = s.trim_matches(|c: char| c <= ' ');
    let (negative, body) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let signed = |v: f64| if negative { -v } else { v };
    match body {
        "NaN" => return Some(f64::NAN),
        "Infinity" => return Some(signed(f64::INFINITY)),
        _ => {}
    }
    let body = body.strip_suffix(['f', 'F', 'd', 'D']).unwrap_or(body);
    if body.starts_with("0x") || body.starts_with("0X") {
        return parse_hex_double(&body[2..]).map(signed);
    }
    let b = body.as_bytes();
    let mut i = 0;
    let int_digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    let int = &body[..int_digits];
    i += int_digits;
    let mut frac = "";
    if b.get(i) == Some(&b'.') {
        i += 1;
        let n = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        frac = &body[i..i + n];
        i += n;
    }
    if int.is_empty() && frac.is_empty() {
        return None;
    }
    let mut exp = "0";
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let start = i + 1;
        i = start;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let exp_digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if exp_digits == 0 {
            return None;
        }
        i += exp_digits;
        exp = &body[start..i];
    }
    if i != b.len() {
        return None;
    }
    // Rust's parser is correctly rounded, as Java's is; rebuild the text in a form it accepts.
    let int = if int.is_empty() { "0" } else { int };
    let frac = if frac.is_empty() { "0" } else { frac };
    format!("{int}.{frac}e{exp}").parse::<f64>().ok().map(signed)
}

/// `0x<hex>[.<hex>]p<exp>`: the binary exponent is mandatory.
fn parse_hex_double(s: &str) -> Option<f64> {
    let (mantissa, exp) = s.split_once(['p', 'P'])?;
    let exp_digits = exp.strip_prefix(['+', '-']).unwrap_or(exp);
    if exp_digits.is_empty() || !exp_digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let exponent = exp.parse::<i64>().unwrap_or(if exp.starts_with('-') {
        i64::MIN / 2
    } else {
        i64::MAX / 2
    });
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if int.is_empty() && frac.is_empty() {
        return None;
    }
    let mut bits = Vec::new();
    for c in int.chars().chain(frac.chars()) {
        let d = c.to_digit(16)?;
        for shift in (0..4).rev() {
            bits.push(d & (1 << shift) != 0);
        }
    }
    let Some(first) = bits.iter().position(|b| *b) else {
        return Some(0.0);
    };
    let bits = &bits[first..];
    let mantissa_high = i64::try_from(bits.len())
        .ok()?
        .checked_sub(1)?
        .checked_sub(i64::try_from(frac.len()).ok()?.checked_mul(4)?)?;
    let mut high = match mantissa_high.checked_add(exponent) {
        Some(high) => high,
        None => return Some(if exponent > 0 { f64::INFINITY } else { 0.0 }),
    };
    if high > 1023 {
        return Some(f64::INFINITY);
    }
    if high < -1075 {
        return Some(0.0);
    }
    let precision = if high >= -1022 {
        53
    } else {
        high.checked_add(1075)? as usize
    };
    let mut significand = 0u64;
    for i in 0..precision {
        significand = (significand << 1) | u64::from(bits.get(i).copied().unwrap_or(false));
    }
    // Round once, directly to the target precision (including subnormals).
    let guard = bits.get(precision).copied().unwrap_or(false);
    let sticky = bits.get(precision + 1..).is_some_and(|tail| tail.iter().any(|b| *b));
    if guard && (sticky || significand & 1 != 0) {
        significand += 1;
    }
    if high < -1022 {
        return Some(f64::from_bits(significand));
    }
    if significand == 1 << 53 {
        significand >>= 1;
        high = high.checked_add(1)?;
    }
    if high > 1023 {
        return Some(f64::INFINITY);
    }
    Some(f64::from_bits(
        ((high.checked_add(1023)? as u64) << 52) | (significand & ((1 << 52) - 1)),
    ))
}

/// `Double.toString` (JDK 19 and later: the shortest decimal that rounds to the value).
pub fn double_to_string(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_owned();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0.0" } else { "0.0" }.to_owned();
    }
    // `{:e}` gives the shortest round-tripping digits: d[.ddd]e<exp>.
    let shortest = format!("{:e}", v.abs());
    let (m, _) = shortest.split_once('e').unwrap_or((&shortest, "0"));
    let mut precision = m.bytes().filter(|b| b.is_ascii_digit()).count();
    if precision == 1 && (v.abs() < 0.001 || v.abs() >= 1e7) {
        precision = 2;
    }
    // Fixed precision uses ties-to-even; Rust's shortest-display path chooses
    // the upper decimal at some exact halfway cases where Java chooses even.
    let rounded = format!("{:.*e}", precision - 1, v.abs());
    let sci = if rounded.parse::<f64>().ok() == Some(v.abs()) {
        rounded
    } else {
        shortest
    };
    render_floating(&sci, v < 0.0)
}

fn render_floating(sci: &str, negative: bool) -> String {
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if negative { "-" } else { "" };
    if (-3..7).contains(&exp) {
        let point = exp.checked_add(1).unwrap_or(0); // digits before the decimal point
        let (int, frac) = if point <= 0 {
            ("0".to_owned(), format!("{}{}", "0".repeat((-point) as usize), digits))
        } else if point as usize >= digits.len() {
            (
                format!("{}{}", digits, "0".repeat(point as usize - digits.len())),
                String::new(),
            )
        } else {
            (digits[..point as usize].to_owned(), digits[point as usize..].to_owned())
        };
        let frac = if frac.is_empty() { "0".to_owned() } else { frac };
        format!("{sign}{int}.{frac}")
    } else {
        let frac = if digits.len() > 1 { &digits[1..] } else { "0" };
        format!("{sign}{}.{}E{}", &digits[..1], frac, exp)
    }
}

/// `Float.toString`, keeping f32 precision rather than widening before printing.
pub fn float_to_string(v: f32) -> String {
    if !v.is_finite() || v == 0.0 {
        return double_to_string(f64::from(v));
    }
    let shortest = format!("{:e}", v.abs());
    let mantissa = shortest.split_once('e').map_or(shortest.as_str(), |(m, _)| m);
    let mut precision = mantissa.bytes().filter(u8::is_ascii_digit).count();
    if precision == 1 && (v.abs() < 0.001 || v.abs() >= 1e7) {
        precision = 2;
    }
    let rounded = format!("{:.*e}", precision - 1, v.abs());
    let sci = if rounded.parse::<f32>().ok() == Some(v.abs()) {
        rounded
    } else {
        shortest
    };
    render_floating(&sci, v < 0.0)
}

/// A `java.math.BigDecimal` as `new BigDecimal(String)` builds it: `unscaled × 10^-scale`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decimal {
    negative: bool,
    /// The unscaled value's digits, without leading zeros ("0" for zero).
    digits: String,
    scale: i32,
}

impl Decimal {
    /// `new BigDecimal(String)`: `[+-]digits[.digits][(e|E)[+-]digits]`, at least one digit in the mantissa.
    pub fn parse(s: &str) -> Option<Decimal> {
        // BigDecimal consumes UTF-16 chars, so only BMP digits normalize.
        let normalized: String = s
            .chars()
            .map(|c| {
                (c as u32 <= 0xffff)
                    .then(|| digit(c as u32, 10))
                    .flatten()
                    .map(|d| (b'0' + d as u8) as char)
                    .unwrap_or(c)
            })
            .collect();
        let s = normalized.as_str();
        let (negative, body) = match s.as_bytes().first() {
            Some(b'-') => (true, &s[1..]),
            Some(b'+') => (false, &s[1..]),
            _ => (false, s),
        };
        let (mantissa, exp) = match body.find(['e', 'E']) {
            Some(i) => (&body[..i], Some(&body[i + 1..])),
            None => (body, None),
        };
        let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        if (int.is_empty() && frac.is_empty()) || !int.bytes().chain(frac.bytes()).all(|c| c.is_ascii_digit()) {
            return None;
        }
        let exp: i64 = match exp {
            None => 0,
            Some(e) => {
                let digits = e.trim_start_matches(['+', '-']);
                if digits.is_empty() || e.len() - digits.len() > 1 || !digits.bytes().all(|c| c.is_ascii_digit()) {
                    return None;
                }
                e.parse().ok()?
            }
        };
        // BigDecimal stores scale as a signed Java int; the exponent itself
        // may exceed that range when fractional digits bring scale back in range.
        let scale = i64::try_from(frac.len()).ok()?.checked_sub(exp)?;
        let scale = i32::try_from(scale).ok()?;
        let all = format!("{int}{frac}");
        i32::try_from(all.len()).ok()?;
        let trimmed = all.trim_start_matches('0');
        Some(Decimal {
            negative,
            digits: if trimmed.is_empty() {
                "0".to_owned()
            } else {
                trimmed.to_owned()
            },
            scale,
        })
    }

    /// BigDecimal.scale(), exposed for binding diagnostics and oracle checks.
    pub fn scale(&self) -> i32 {
        self.scale
    }

    fn is_zero(&self) -> bool {
        self.digits == "0"
    }

    /// `precision() - scale()` of `this.scaleByPowerOfTen(9)`: the number of digits left of the point (may be <= 0).
    fn integer_digits_after_nano_scaling(&self) -> Option<i64> {
        let precision = i64::try_from(self.digits.len()).ok()?;
        precision.checked_sub(i64::from(self.scale).checked_sub(9)?)
    }

    /// `BigDecimal.longValue()` (low 64 bits of the integer part) and the truncated fraction in nanoseconds, signed.
    fn seconds_and_nanos(&self) -> Option<(i64, i64)> {
        let digits = self.digits.as_bytes();
        let int_len = i64::try_from(digits.len()).ok()?.checked_sub(i64::from(self.scale))?;
        let mut int: u64 = 0;
        for k in 0..int_len.max(0) {
            let d = digits.get(k as usize).map_or(0, |c| u64::from(c - b'0'));
            int = int.wrapping_mul(10).wrapping_add(d);
        }
        let mut nanos: i64 = 0;
        for k in int_len..int_len.checked_add(9)? {
            let d = if k < 0 {
                0
            } else {
                digits.get(k as usize).map_or(0, |c| i64::from(c - b'0'))
            };
            nanos = nanos * 10 + d;
        }
        if self.negative {
            Some((int.wrapping_neg() as i64, -nanos))
        } else {
            Some((int as i64, nanos))
        }
    }

    /// D9: exact decimal nanoseconds, rejecting an integer part outside i64.
    pub fn checked_seconds_and_nanos(&self) -> Option<(i64, i64)> {
        if self.is_zero() {
            return Some((0, 0));
        }
        let int_len = i64::try_from(self.digits.len())
            .ok()?
            .checked_sub(i64::from(self.scale))?;
        if int_len > 19 {
            return None;
        }
        let mut seconds = 0i64;
        for k in 0..int_len.max(0) {
            let digit = self
                .digits
                .as_bytes()
                .get(k as usize)
                .map_or(0, |c| i64::from(c - b'0'));
            seconds = seconds.checked_mul(10)?.checked_sub(digit)?;
        }
        if !self.negative {
            seconds = seconds.checked_neg()?;
        }
        let mut nanos = 0i64;
        for k in int_len..int_len.checked_add(9)? {
            let digit = usize::try_from(k)
                .ok()
                .and_then(|i| self.digits.as_bytes().get(i))
                .map_or(0, |c| i64::from(c - b'0'));
            nanos = nanos * 10 + digit;
        }
        if self.negative && !(seconds < 0 && seconds > crate::jtime::INSTANT_MIN_SECONDS) {
            nanos = -nanos;
        }
        Some((seconds, nanos))
    }

    /// `DecimalUtils.extractSecondsAndNanos` (jackson-databind 2.17), including its sign handling: for negative whole
    /// seconds the nanos are made positive, so -1.5 becomes (-1, 500000000), which is -0.5 s.
    pub fn extract_seconds_and_nanos(&self) -> (i64, i64) {
        if self.is_zero() || self.integer_digits_after_nano_scaling().is_none_or(|n| n <= 0) || self.scale < -63 {
            return (0, 0);
        }
        let Some((seconds, mut nanos)) = self.seconds_and_nanos() else {
            return (0, 0);
        };
        if seconds < 0 && seconds > crate::jtime::INSTANT_MIN_SECONDS {
            nanos = nanos.abs();
        }
        (seconds, nanos)
    }
}

/// BigInteger.longValue(): retain low 64 bits of a validated decimal integer.
pub fn bigint_long_value(decimal: &str) -> i64 {
    let negative = decimal.starts_with('-');
    let mut bits = 0u64;
    for digit in decimal.trim_start_matches(['-', '+']).bytes() {
        bits = bits.wrapping_mul(10).wrapping_add(u64::from(digit - b'0'));
    }
    if negative {
        bits.wrapping_neg() as i64
    } else {
        bits as i64
    }
}

/// Jackson NumberInput.parseAsInt: decimal strings may truncate through Double;
/// plain integer overflow returns the default, rather than narrowing or saturating.
pub fn jackson_parse_as_int(text: &str) -> i32 {
    let text = text.trim_matches(|c: char| c <= ' ');
    if text.trim_start_matches(['-', '+']).bytes().all(|b| b.is_ascii_digit()) {
        parse_long(text).and_then(|n| i32::try_from(n).ok()).unwrap_or(0)
    } else {
        parse_double(text).unwrap_or(0.0) as i32
    }
}

/// Jackson NumberInput.parseAsLong, with the same non-throwing default as JsonNode.
pub fn jackson_parse_as_long(text: &str) -> i64 {
    let text = text.trim_matches(|c: char| c <= ' ');
    if text.trim_start_matches(['-', '+']).bytes().all(|b| b.is_ascii_digit()) {
        parse_long(text).unwrap_or(0)
    } else {
        parse_double(text).unwrap_or(0.0) as i64
    }
}

/// `Double.compare` (what `Comparator.comparingDouble` uses): numeric order, then
/// `doubleToLongBits` order, so `-0.0 < 0.0` and every NaN equals every other NaN
/// and sorts above `+Infinity`. `f64::total_cmp` differs: it orders NaNs by their
/// sign and payload bits, and a NaN's sign bit is platform-dependent.
pub fn double_compare(a: f64, b: f64) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if a < b {
        return Ordering::Less;
    }
    if a > b {
        return Ordering::Greater;
    }
    let bits = |v: f64| {
        if v.is_nan() {
            0x7ff8_0000_0000_0000_i64
        } else {
            v.to_bits() as i64
        }
    };
    bits(a).cmp(&bits(b))
}

/// Influx Point's English NumberFormat on integral doubles (duration fields).
/// It uses decimal notation and at least one fractional digit, never an exponent.
pub fn influx_duration(value: i64) -> String {
    let text = double_to_string(value as f64);
    let Some((mantissa, exponent)) = text.split_once('E') else {
        return text;
    };
    let exponent: usize = exponent.parse().unwrap_or(0);
    let (sign, mantissa) = mantissa.strip_prefix('-').map_or(("", mantissa), |m| ("-", m));
    let digits = mantissa.replace('.', "");
    let integer_digits = exponent + 1;
    let mut out = String::from(sign);
    out.push_str(&digits);
    if integer_digits >= digits.len() {
        out.push_str(&"0".repeat(integer_digits - digits.len()));
        out.push_str(".0");
    } else {
        out.insert(sign.len() + integer_digits, '.');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_compare_matches_java() {
        use std::cmp::Ordering::*;
        let negative_nan = f64::from_bits(0xfff8_0000_0000_0001);
        for (a, b, expected) in [
            (-0.0, 0.0, Less),
            (0.0, -0.0, Greater),
            (0.0, 0.0, Equal),
            (-0.0, -0.0, Equal),
            (f64::NAN, f64::NAN, Equal),
            (f64::NAN, negative_nan, Equal),
            (negative_nan, f64::INFINITY, Greater),
            (f64::INFINITY, f64::NAN, Less),
            (f64::NEG_INFINITY, -1.0, Less),
            (1500.0, 1000.0, Greater),
            (0.1, 0.2, Less),
        ] {
            assert_eq!(double_compare(a, b), expected, "{a} vs {b}");
        }
        // The case that rules out total_cmp: a negative-signed NaN still sorts last.
        assert_eq!(negative_nan.total_cmp(&f64::INFINITY), Less);
    }

    #[test]
    fn big_decimal_scale_limits() {
        for (text, scale) in [
            ("1e-2147483647", i32::MAX),
            ("1e2147483648", i32::MIN),
            ("1.0e2147483649", i32::MIN),
            ("1e000000000000000000000001", -1),
        ] {
            let decimal = Decimal::parse(text).unwrap();
            assert_eq!(decimal.scale, scale, "{text}");
            assert_eq!(
                decimal.extract_seconds_and_nanos(),
                if scale == -1 { (10, 0) } else { (0, 0) }
            );
        }
        for text in [
            "1e-9223372036854775808",
            "1e9223372036854775807",
            "1e9223372036854775808",
            "1e-2147483648",
            "1e2147483649",
            "1.0e-2147483647",
        ] {
            assert!(Decimal::parse(text).is_none(), "{text}");
        }
    }

    #[test]
    fn seeded_number_parse_no_panics() {
        let mut seed = 0x03b3_5eed_u64;
        let alphabet: Vec<char> = "0123456789abcdefABCDEFxXpPeE+-. fFdD∞١９𝟙\0".chars().collect();
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as usize
        };
        for _ in 0..100_000 {
            let len = next() % 96;
            let text: String = (0..len).map(|_| alphabet[next() % alphabet.len()]).collect();
            for input in [text.clone(), format!("1e{text}"), format!("0x1p{text}")] {
                let _ = parse_long(&input);
                let _ = parse_radix(&input, 16);
                let _ = parse_double(&input);
                if let Some(decimal) = Decimal::parse(&input) {
                    let _ = decimal.extract_seconds_and_nanos();
                }
            }
        }
        for exp in [
            i64::MIN,
            i64::MIN + 1,
            i32::MIN as i64,
            i32::MAX as i64,
            i64::MAX - 1,
            i64::MAX,
        ] {
            for mantissa in ["0", "1", "-1", "1.0", "0.001"] {
                for input in [format!("{mantissa}e{exp}"), format!("0x{mantissa}p{exp}")] {
                    let _ = parse_double(&input);
                    if let Some(decimal) = Decimal::parse(&input) {
                        let _ = decimal.extract_seconds_and_nanos();
                    }
                }
            }
        }
    }

    #[test]
    fn java_double_strings() {
        for (v, s) in [
            (1.5, "1.5"),
            (1000.0, "1000.0"),
            (1.5e10, "1.5E10"),
            (685230.15, "685230.15"),
            (0.001, "0.001"),
            (0.0001, "1.0E-4"),
            (1e7, "1.0E7"),
            (9999999.0, "9999999.0"),
            (-0.5, "-0.5"),
            (9.223372036854776e18, "9.223372036854776E18"),
        ] {
            assert_eq!(double_to_string(v), s);
        }
    }

    #[test]
    fn java_parse_double() {
        assert_eq!(parse_double("5."), Some(5.0));
        assert_eq!(parse_double(".5"), Some(0.5));
        assert_eq!(parse_double(" 1e3d "), Some(1000.0));
        assert_eq!(parse_double("+1.5E+3"), Some(1500.0));
        assert_eq!(parse_double("-Infinity"), Some(f64::NEG_INFINITY));
        assert_eq!(parse_double("0x1p3"), Some(8.0));
        for bad in [".inf", "1:20.5", "x", ".", "1e", "inf", "nan", "1_0"] {
            assert_eq!(parse_double(bad), None, "{bad}");
        }
    }

    #[test]
    fn parse_int_matches_integer_parse_int() {
        // Reference JBR oracle, WP 6.3.
        assert_eq!(parse_int("42"), Some(42));
        assert_eq!(parse_int("+42"), Some(42));
        assert_eq!(parse_int("-2147483648"), Some(i32::MIN));
        assert_eq!(parse_int("٤٢"), Some(42)); // Arabic-Indic digits via Character.digit
        for bad in [" 42", "2147483648", "", "+", "4.2"] {
            assert_eq!(parse_int(bad), None, "{bad}");
        }
    }

    #[test]
    fn decode_int_matches_integer_decode() {
        // Reference JBR oracle, WP 6.3.
        assert_eq!(decode_int("0"), Some(0));
        assert_eq!(decode_int("010"), Some(8));
        assert_eq!(decode_int("0x1A"), Some(26));
        assert_eq!(decode_int("#1A"), Some(26));
        assert_eq!(decode_int("-0x10"), Some(-16));
        assert_eq!(decode_int("-#10"), Some(-16));
        assert_eq!(decode_int("+0x10"), Some(16));
        assert_eq!(decode_int("-0x80000000"), Some(i32::MIN));
        assert_eq!(decode_int("00"), Some(0));
        for bad in [
            "08",
            "abc",
            "0x",
            "#",
            "",
            "0x80000000",
            "2147483648",
            "0b101",
            " 10",
            "--10",
            "+-10",
        ] {
            assert_eq!(decode_int(bad), None, "{bad}");
        }
    }

    #[test]
    fn seconds_and_nanos() {
        let s = |t: &str| Decimal::parse(t).unwrap().extract_seconds_and_nanos();
        assert_eq!(s("1.5"), (1, 500_000_000));
        assert_eq!(s("-1.5"), (-1, 500_000_000));
        assert_eq!(s("-0.5"), (0, -500_000_000));
        assert_eq!(s("1.5e10"), (15_000_000_000, 0));
        assert_eq!(s("6.8523015e+5"), (685_230, 150_000_000));
        assert_eq!(s("1e-10"), (0, 0));
    }
}

/// Kotlin Float.roundToInt: nearest integer, ties towards positive infinity;
/// saturates infinities/overflow and rejects NaN.
pub fn kotlin_round_to_int(value: f32) -> Option<i32> {
    if value.is_nan() {
        None
    } else {
        Some((f64::from(value) + 0.5).floor() as i32)
    }
}

#[cfg(test)]
mod kotlin_round_tests {
    #[test]
    fn float_round_to_int_keeps_jvm_ties_and_saturation() {
        assert_eq!(super::kotlin_round_to_int(-0.5), Some(0));
        assert_eq!(super::kotlin_round_to_int(0.5), Some(1));
        assert_eq!(super::kotlin_round_to_int(f32::INFINITY), Some(i32::MAX));
        assert_eq!(super::kotlin_round_to_int(f32::NEG_INFINITY), Some(i32::MIN));
        assert_eq!(super::kotlin_round_to_int(f32::NAN), None);
        assert_eq!(super::kotlin_round_to_int(16_777_216.0), Some(16_777_216));
    }
}

/// Kotlin Double.roundToLong: ties toward positive infinity, saturating infinities.
/// NaN throws IllegalArgumentException at the caller's boundary.
pub fn kotlin_round_to_long(value: f64) -> Option<i64> {
    if value.is_nan() {
        return None;
    }
    let floor = value.floor();
    Some(if value - floor >= 0.5 {
        (floor as i64).saturating_add(1)
    } else {
        floor as i64
    })
}
