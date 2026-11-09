//! Port of configuration/src/main/kotlin/com/malinskiy/marathon/config/serialization/yaml/SerializeModule.kt (Java library semantics).
//! Promoted from WP 0.3b, `spikes/yaml-node/src/jtime.rs`; retains its checked arithmetic.
//! `java.time.Instant` and `java.time.Duration`: the parsing and `toString` the Instant and Duration targets need.

/// `Instant.MIN.getEpochSecond()`.
pub const INSTANT_MIN_SECONDS: i64 = -31_557_014_167_219_200;
/// `Instant.MAX.getEpochSecond()`.
pub const INSTANT_MAX_SECONDS: i64 = 31_556_889_864_403_199;
const NANOS: i64 = 1_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Instant {
    seconds: i64,
    nanos: i64,
}

impl Instant {
    /// Java Instant.EPOCH.
    pub const EPOCH: Self = Self { seconds: 0, nanos: 0 };

    /// Java Instant.toEpochMilli, including the negative-second overflow boundary.
    pub fn to_epoch_millis(&self) -> Option<i64> {
        if self.seconds < 0 && self.nanos > 0 {
            (self.seconds + 1)
                .checked_mul(1000)?
                .checked_add(self.nanos / 1_000_000 - 1000)
        } else {
            self.seconds.checked_mul(1000)?.checked_add(self.nanos / 1_000_000)
        }
    }

    pub fn seconds(&self) -> i64 {
        self.seconds
    }
    pub fn nanos(&self) -> i64 {
        self.nanos
    }
    /// `Instant.plus(Duration)`; `None` is the `DateTimeException`/`ArithmeticException`
    /// on overflow or a result outside `Instant.MIN..=MAX` (WP 2.2 S7).
    pub fn checked_add(&self, duration: Duration) -> Option<Instant> {
        instant_of(self.seconds.checked_add(duration.seconds)?, self.nanos + duration.nanos)
    }
}

/// `instant.plus(duration)`; free-function form of [`Instant::checked_add`].
pub fn instant_plus_duration(instant: Instant, duration: Duration) -> Option<Instant> {
    instant.checked_add(duration)
}

/// `Instant.ofEpochSecond(seconds, nanoAdjustment)`; `None` is the `DateTimeException`/`ArithmeticException`.
pub fn instant_of(seconds: i64, nano_adjustment: i64) -> Option<Instant> {
    let s = seconds.checked_add(nano_adjustment.div_euclid(NANOS))?;
    let n = nano_adjustment.rem_euclid(NANOS);
    if !(INSTANT_MIN_SECONDS..=INSTANT_MAX_SECONDS).contains(&s) {
        return None;
    }
    Some(Instant { seconds: s, nanos: n })
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian.
pub(crate) fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn month_length(y: i64, m: i64) -> i64 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Nanoseconds as `DateTimeFormatter.ISO_INSTANT` prints them: groups of three digits, as many as needed.
fn fraction(nanos: i64) -> String {
    if nanos == 0 {
        return String::new();
    }
    let s = format!("{nanos:09}");
    let n = if nanos % 1_000_000 == 0 {
        3
    } else if nanos % 1000 == 0 {
        6
    } else {
        9
    };
    format!(".{}", &s[..n])
}

impl std::fmt::Display for Instant {
    /// `Instant.toString()` (ISO_INSTANT).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let days = self.seconds.div_euclid(86_400);
        let secs = self.seconds.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        let year = if y > 9999 {
            format!("+{y}")
        } else if y < 0 {
            format!("-{:04}", -y)
        } else {
            format!("{y:04}")
        };
        write!(
            f,
            "{year}-{m:02}-{d:02}T{:02}:{:02}:{:02}{}Z",
            secs / 3600,
            secs % 3600 / 60,
            secs % 60,
            fraction(self.nanos)
        )
    }
}

/// `DateTimeFormatter.ISO_INSTANT.parse` then `Instant.from` (case-insensitive, strict, offsets allowed).
pub fn parse_instant(s: &str) -> Option<Instant> {
    parse_instant_detailed(s).ok()
}

/// `Instant.parse` failure: the `DateTimeParseException` first, then its causes, each as (class, message).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstantParseError {
    pub chain: Vec<(marathon_error::FailureKind, String)>,
}

const PARSE_EXCEPTION: marathon_error::FailureKind = marathon_error::FailureKind::DateTimeParse;
const DATE_TIME_EXCEPTION: marathon_error::FailureKind = marathon_error::FailureKind::DateTime;

/// Why the composite parser stopped: `~index`, or a `RuntimeException` thrown from inside a sub-parser.
enum Stop {
    At(usize),
    Thrown(&'static str),
}

/// `Instant.parse` with the JDK's exception: `DateTimeFormatter.parseResolved0` reports the sub-parser's error index,
/// then unparsed text; a resolve failure is wrapped by `createError`. The text is measured in UTF-16 units; every
/// unit consumed before an error is ASCII.
pub fn parse_instant_detailed(s: &str) -> Result<Instant, InstantParseError> {
    let t: Vec<u16> = s.encode_utf16().collect();
    let abbr = if t.len() > 64 {
        crate::jstring::take_utf16_complete(s, 64).to_owned() + "..."
    } else {
        s.to_owned()
    };
    let text = |tail: &str| format!("Text '{abbr}{tail}");
    match parse_iso_instant(&t) {
        Err(Stop::At(index)) => Err(InstantParseError {
            chain: vec![(
                PARSE_EXCEPTION,
                text(&format!("' could not be parsed at index {index}")),
            )],
        }),
        Err(Stop::Thrown(message)) => Err(InstantParseError {
            chain: vec![
                (PARSE_EXCEPTION, text(&format!("' could not be parsed: {message}"))),
                (DATE_TIME_EXCEPTION, String::from(message)),
            ],
        }),
        Ok((end, _)) if end < t.len() => Err(InstantParseError {
            chain: vec![(
                PARSE_EXCEPTION,
                text(&format!("' could not be parsed, unparsed text found at index {end}")),
            )],
        }),
        Ok((_, (seconds, nanos))) => instant_of(seconds, nanos).ok_or_else(|| {
            // `Instant.from` wraps `ofEpochSecond`'s range error and prints the `Parsed` map, whose `HashMap` order
            // follows the fields' identity hashes and so varies between JVM runs; this is one of those orders.
            let unable = format!(
                "Unable to obtain Instant from TemporalAccessor: {{MilliOfSecond={}, InstantSeconds={seconds}, \
                 NanoOfSecond={nanos}, MicroOfSecond={}}},ISO of type java.time.format.Parsed",
                nanos / 1_000_000,
                nanos / 1_000
            );
            InstantParseError {
                chain: vec![
                    (PARSE_EXCEPTION, text(&format!("' could not be parsed: {unable}"))),
                    (DATE_TIME_EXCEPTION, String::from(unable.as_str())),
                    (
                        DATE_TIME_EXCEPTION,
                        String::from("Instant exceeds minimum or maximum instant"),
                    ),
                ],
            }
        }),
    }
}

/// `InstantPrinterParser.parse` from index 0: ISO_LOCAL_DATE 'T' HH ':' mm ':' ss [fraction] offset-id, then the
/// `LocalDateTime` check, whose failure is `~0`. Returns the end index and (epoch seconds, nanos).
fn parse_iso_instant(t: &[u16]) -> Result<(usize, (i64, i64)), Stop> {
    let digit = |i: usize| {
        t.get(i)
            .filter(|c| (0x30..=0x39).contains(*c))
            .map(|c| i64::from(*c - 0x30))
    };
    // `NumberPrinterParser` in strict mode: `min..=max` digits, else `~position`.
    let number = |position: usize, min: usize, max: usize| -> Result<(usize, i64), Stop> {
        let mut pos = position;
        let mut total = 0;
        while pos < position + max {
            match digit(pos) {
                Some(d) => total = total * 10 + d,
                None => break,
            }
            pos += 1;
        }
        if pos - position < min {
            Err(Stop::At(position))
        } else {
            Ok((pos, total))
        }
    };
    // `CharLiteralPrinterParser`, case-insensitive (only ASCII letters fold to `T` or `Z`).
    let literal = |pos: usize, c: u8| -> Result<usize, Stop> {
        match t.get(pos) {
            Some(&u) if u < 0x80 && (u as u8).eq_ignore_ascii_case(&c) => Ok(pos + 1),
            _ => Err(Stop::At(pos)),
        }
    };
    // `appendValue(field, 2)`: SignStyle.NOT_NEGATIVE rejects a sign at the field's start in strict mode.
    let two = |pos: usize| number(pos, 2, 2);

    // YEAR: 4 to 10 digits, SignStyle.EXCEEDS_PAD.
    let (start, sign) = match t.first() {
        Some(0x2B) => (1, 1),
        Some(0x2D) => (1, -1),
        _ => (0, 0),
    };
    let (pos, year) = number(start, 4, 10)?;
    let year_digits = pos - start;
    if (sign == -1 && year == 0) || (sign == 1 && year_digits <= 4) {
        return Err(Stop::At(start - 1));
    }
    if sign == 0 && year_digits > 4 {
        return Err(Stop::At(start));
    }
    let year = if sign == -1 { -year } else { year };
    let pos = literal(pos, b'-')?;
    let (pos, month) = two(pos)?;
    let pos = literal(pos, b'-')?;
    let (pos, day) = two(pos)?;
    let pos = literal(pos, b'T')?;
    let (pos, mut hour) = two(pos)?;
    let pos = literal(pos, b':')?;
    let (pos, minute) = two(pos)?;
    let pos = literal(pos, b':')?;
    let (mut pos, mut second) = two(pos)?;
    let mut nanos = 0;
    if t.get(pos) == Some(&0x2E) {
        let (end, n) = number(pos + 1, 0, 9)?;
        nanos = n * 10i64.pow(9 - (end - pos - 1) as u32);
        pos = end;
    }
    let (pos, offset) = offset_id(t, pos)?;

    let mut extra_days = 0;
    if hour == 24 && minute == 0 && second == 0 && nanos == 0 {
        hour = 0;
        extra_days = 1;
    } else if hour == 23 && minute == 59 && second == 60 {
        second = 59;
    }
    // `LocalDateTime.of(year % 10_000, …)` and `ZoneOffset.ofTotalSeconds`; leap years repeat every 400 years, so
    // the remainder's calendar is the year's.
    if !(1..=12).contains(&month)
        || day < 1
        || day > month_length(year, month)
        || hour > 23
        || minute > 59
        || second > 59
        || offset.abs() > 18 * 3600
    {
        return Err(Stop::At(0));
    }
    let days = days_from_civil(year, month, day) + extra_days;
    Ok((
        pos,
        (days * 86_400 + hour * 3600 + minute * 60 + second - offset, nanos),
    ))
}

/// `OffsetIdPrinterParser("+HH:MM:ss", "Z")`: `~position` unless `Z` or a sign, two hour digits and `:MM` follow
/// (each 0..=59); `:ss` is optional. Hours 24..=59 throw from inside the parser.
fn offset_id(t: &[u16], position: usize) -> Result<(usize, i64), Stop> {
    let pair = |pos: usize| -> Option<i64> {
        let d = |i: usize| {
            t.get(i)
                .filter(|c| (0x30..=0x39).contains(*c))
                .map(|c| i64::from(*c - 0x30))
        };
        let value = d(pos)? * 10 + d(pos + 1)?;
        (value <= 59).then_some(value)
    };
    let colon_pair = |pos: usize| if t.get(pos) == Some(&0x3A) { pair(pos + 1) } else { None };
    let sign = match t.get(position) {
        None => return Err(Stop::At(position)),
        Some(&u) if u == 0x5A || u == 0x7A => return Ok((position + 1, 0)),
        Some(0x2B) => 1,
        Some(0x2D) => -1,
        Some(_) => return Err(Stop::At(position)),
    };
    let (Some(h), Some(m)) = (pair(position + 1), colon_pair(position + 3)) else {
        return Err(Stop::At(position));
    };
    let (end, sec) = match colon_pair(position + 6) {
        Some(sec) => (position + 9, sec),
        None => (position + 6, 0),
    };
    if h > 23 {
        return Err(Stop::Thrown(
            "Value out of range: Hour[0-23], Minute[0-59], Second[0-59]",
        ));
    }
    Ok((end, sign * (h * 3600 + m * 60 + sec)))
}

/// The derived `Ord` matches `Duration.compareTo` because `nanos` is
/// always normalised to `0..1e9`, so `(seconds, nanos)` lexicographic
/// order is the numeric order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Duration {
    seconds: i64,
    /// 0..1e9, always added to `seconds` (as in `java.time.Duration`).
    nanos: i64,
}

impl Duration {
    pub const ZERO: Self = Self { seconds: 0, nanos: 0 };

    pub const fn of_seconds(seconds: i64) -> Self {
        Self { seconds, nanos: 0 }
    }

    /// `Duration.isNegative()`.
    pub fn is_negative(self) -> bool {
        self < Self::ZERO
    }

    /// `Duration.isZero()`.
    pub fn is_zero(self) -> bool {
        self == Self::ZERO
    }

    /// `Duration.multipliedBy(multiplicand)`; `None` is the
    /// `ArithmeticException` on overflow.
    pub fn checked_mul(self, multiplicand: i64) -> Option<Self> {
        let total = (i128::from(self.seconds) * i128::from(NANOS) + i128::from(self.nanos))
            .checked_mul(i128::from(multiplicand))?;
        let seconds = i64::try_from(total.div_euclid(i128::from(NANOS))).ok()?;
        Some(Self {
            seconds,
            nanos: total.rem_euclid(i128::from(NANOS)) as i64,
        })
    }

    /// Java Duration.toMillis(), truncating toward zero and rejecting overflow.
    pub fn to_millis(&self) -> Option<i64> {
        i64::try_from((i128::from(self.seconds) * 1_000_000_000 + i128::from(self.nanos)) / 1_000_000).ok()
    }
    pub fn seconds(&self) -> i64 {
        self.seconds
    }
    pub fn nanos(&self) -> i64 {
        self.nanos
    }
}

/// `Duration.ofSeconds(seconds, nanoAdjustment)`.
pub fn duration_of(seconds: i64, nano_adjustment: i64) -> Option<Duration> {
    let s = seconds.checked_add(nano_adjustment.div_euclid(NANOS))?;
    Some(Duration {
        seconds: s,
        nanos: nano_adjustment.rem_euclid(NANOS),
    })
}

impl Duration {
    fn negated(self) -> Option<Duration> {
        // multipliedBy(-1), exact.
        let total = i128::from(self.seconds) * i128::from(NANOS) + i128::from(self.nanos);
        let neg = -total;
        let s = i64::try_from(neg.div_euclid(i128::from(NANOS))).ok()?;
        Some(Duration {
            seconds: s,
            nanos: neg.rem_euclid(i128::from(NANOS)) as i64,
        })
    }
}

impl std::fmt::Display for Duration {
    /// `Duration.toString()`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.seconds == 0 && self.nanos == 0 {
            return f.write_str("PT0S");
        }
        let mut effective = self.seconds;
        if self.seconds < 0 && self.nanos > 0 {
            effective += 1;
        }
        let hours = effective / 3600;
        let minutes = (effective % 3600) / 60;
        let secs = effective % 60;
        let mut buf = String::from("PT");
        if hours != 0 {
            buf.push_str(&format!("{hours}H"));
        }
        if minutes != 0 {
            buf.push_str(&format!("{minutes}M"));
        }
        if secs == 0 && self.nanos == 0 && buf.len() > 2 {
            return f.write_str(&buf);
        }
        if self.seconds < 0 && self.nanos > 0 {
            if secs == 0 {
                buf.push_str("-0");
            } else {
                buf.push_str(&secs.to_string());
            }
        } else {
            buf.push_str(&secs.to_string());
        }
        if self.nanos > 0 {
            let pos = buf.len();
            if self.seconds < 0 {
                buf.push_str(&(2 * NANOS - self.nanos).to_string());
            } else {
                buf.push_str(&(self.nanos + NANOS).to_string());
            }
            while buf.ends_with('0') {
                buf.pop();
            }
            buf.replace_range(pos..pos + 1, ".");
        }
        buf.push('S');
        f.write_str(&buf)
    }
}

/// `Duration.parse` (Java 21): `[-+]?P(n D)?(T(n H)?(n M)?(n([.,]f{0,9})? S)?)?`, case-insensitive.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let b = s.as_bytes();
    let mut i = 0;
    let negate = match b.first() {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    if b.get(i).map(u8::to_ascii_uppercase) != Some(b'P') {
        return None;
    }
    i += 1;
    // A signed number followed by `unit`; returns (text of the number, position after the unit).
    let number = |from: usize, unit: u8| -> Option<(usize, usize, usize)> {
        let mut j = from;
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        let d = b[j..].iter().take_while(|c| c.is_ascii_digit()).count();
        if d == 0 {
            return None;
        }
        let end = j + d;
        if b.get(end).map(u8::to_ascii_uppercase) == Some(unit) {
            Some((from, end, end + 1))
        } else {
            None
        }
    };
    let mut days = None;
    if let Some((a, e, next)) = number(i, b'D') {
        days = Some((a, e));
        i = next;
    }
    let (mut hours, mut minutes, mut seconds, mut fraction) = (None, None, None, None);
    let mut has_t = false;
    if b.get(i).map(u8::to_ascii_uppercase) == Some(b'T') {
        has_t = true;
        i += 1;
        if let Some((a, e, next)) = number(i, b'H') {
            hours = Some((a, e));
            i = next;
        }
        if let Some((a, e, next)) = number(i, b'M') {
            minutes = Some((a, e));
            i = next;
        }
        // Seconds: n([.,]f{0,9})?S
        let mut j = i;
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        let d = b[j..].iter().take_while(|c| c.is_ascii_digit()).count();
        if d > 0 {
            let mut end = j + d;
            let mut frac = None;
            if matches!(b.get(end), Some(b'.' | b',')) {
                let f = b[end + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
                if f <= 9 {
                    frac = Some((end + 1, end + 1 + f));
                    end = end + 1 + f;
                } else {
                    return None;
                }
            }
            if b.get(end).map(u8::to_ascii_uppercase) == Some(b'S') {
                seconds = Some((i, j + d));
                fraction = frac;
                i = end + 1;
            }
        }
    }
    if i != b.len() {
        return None;
    }
    if has_t && hours.is_none() && minutes.is_none() && seconds.is_none() {
        return None;
    }
    if days.is_none() && hours.is_none() && minutes.is_none() && seconds.is_none() {
        return None;
    }
    let part = |r: Option<(usize, usize)>, mul: i64| -> Option<i64> {
        match r {
            None => Some(0),
            Some((a, e)) => crate::jnum::parse_long(&s[a..e])?.checked_mul(mul),
        }
    };
    let d = part(days, 86_400)?;
    let h = part(hours, 3600)?;
    let m = part(minutes, 60)?;
    let sec = part(seconds, 1)?;
    let negative_secs = seconds.is_some_and(|(a, _)| b[a] == b'-');
    let nanos = match fraction {
        Some((a, e)) if e > a => {
            let v: i64 = s[a..e].parse().ok()?;
            v * 10i64.pow(9 - (e - a) as u32) * if negative_secs { -1 } else { 1 }
        }
        _ => 0,
    };
    let total = d.checked_add(h.checked_add(m.checked_add(sec)?)?)?;
    let dur = duration_of(total, nanos)?;
    if negate { dur.negated() } else { Some(dur) }
}

/// Instant.toEpochMilli(), with floor rounding before the epoch and checked Long range.
pub fn instant_to_epoch_milli(instant: Instant) -> Option<i64> {
    i64::try_from(i128::from(instant.seconds) * 1000 + i128::from(instant.nanos / 1_000_000)).ok()
}

/// Duration.between(Instant, Instant).toMillis(): truncate the signed interval.
pub fn instant_duration_millis(start: Instant, end: Instant) -> Option<i64> {
    let nanos = (i128::from(end.seconds) - i128::from(start.seconds)) * 1_000_000_000 + i128::from(end.nanos)
        - i128::from(start.nanos);
    i64::try_from(nanos / 1_000_000).ok()
}

/// InfluxDB v2 NanosecondConverter's BigInteger division truncates toward zero,
/// unlike Instant.toEpochMilli() for a negative fractional millisecond.
pub fn influx_epoch_millis(instant: Instant) -> i128 {
    (i128::from(instant.seconds()) * 1_000_000_000 + i128::from(instant.nanos())) / 1_000_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instant_error_abbreviation_keeps_complete_characters() {
        for (input, abbreviation) in [
            (format!("{}😀", "a".repeat(63)), format!("{}...", "a".repeat(63))),
            (format!("{}😀z", "a".repeat(62)), format!("{}😀...", "a".repeat(62))),
            (format!("{}😀", "a".repeat(62)), format!("{}😀", "a".repeat(62))),
            (format!("{}�z", "a".repeat(63)), format!("{}�...", "a".repeat(63))),
        ] {
            assert_eq!(
                parse_instant_detailed(&input).unwrap_err().chain,
                vec![(
                    PARSE_EXCEPTION,
                    format!("Text '{abbreviation}' could not be parsed at index 0")
                )]
            );
        }
    }

    #[test]
    fn instants() {
        let p = |s: &str| parse_instant(s).map(|i| i.to_string());
        assert_eq!(p("2001-12-15T02:59:43.1Z").as_deref(), Some("2001-12-15T02:59:43.100Z"));
        assert_eq!(p("2026-01-01T00:00:00Z").as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(p("2001-12-15"), None);
        assert_eq!(
            instant_of(1_000_000, 500_000_000).unwrap().to_string(),
            "1970-01-12T13:46:40.500Z"
        );
        assert_eq!(
            instant_of(-1, 500_000_000).unwrap().to_string(),
            "1969-12-31T23:59:59.500Z"
        );
        assert_eq!(
            instant_of(15_000_000_000, 0).unwrap().to_string(),
            "2445-05-01T02:40:00Z"
        );
    }

    #[test]
    fn instant_plus_duration_checked() {
        let now = parse_instant("2026-10-12T00:00:00Z").unwrap();
        assert_eq!(
            now.checked_add(parse_duration("-PT1H").unwrap()).unwrap().to_string(),
            "2026-10-11T23:00:00Z"
        );
        assert_eq!(
            instant_plus_duration(now, parse_duration("-P30D").unwrap())
                .unwrap()
                .to_string(),
            "2026-09-12T00:00:00Z"
        );
        assert_eq!(
            now.checked_add(duration_of(0, 1_500_000_000).unwrap())
                .unwrap()
                .to_string(),
            "2026-10-12T00:00:01.500Z"
        );
        let max = instant_of(INSTANT_MAX_SECONDS, 999_999_999).unwrap();
        assert_eq!(max.checked_add(duration_of(1, 0).unwrap()), None);
        assert_eq!(max.checked_add(duration_of(0, 1).unwrap()), None);
        let min = instant_of(INSTANT_MIN_SECONDS, 0).unwrap();
        assert_eq!(min.checked_add(duration_of(-1, 0).unwrap()), None);
        assert_eq!(now.checked_add(duration_of(i64::MAX, 0).unwrap()), None);
    }

    #[test]
    fn durations() {
        let p = |s: &str| parse_duration(s).map(|d| d.to_string());
        assert_eq!(p("PT30S").as_deref(), Some("PT30S"));
        assert_eq!(p("-PT1H").as_deref(), Some("PT-1H"));
        assert_eq!(p("P1D").as_deref(), Some("PT24H"));
        assert_eq!(p("PT"), None);
        assert_eq!(p("30"), None);
        assert_eq!(duration_of(-1, 500_000_000).unwrap().to_string(), "PT-0.5S");
        assert_eq!(
            duration_of(i64::MIN, 0).unwrap().to_string(),
            "PT-2562047788015215H-30M-8S"
        );
        assert_eq!(
            duration_of(685_230, 150_000_000).unwrap().to_string(),
            "PT190H20M30.15S"
        );
    }

    #[test]
    fn duration_order_sign_and_checked_mul() {
        let neg_half = duration_of(-1, 500_000_000).unwrap();
        assert!(neg_half.is_negative());
        assert!(!neg_half.is_zero());
        assert!(Duration::ZERO.is_zero());
        assert!(!Duration::ZERO.is_negative());
        assert!(neg_half < Duration::ZERO);
        assert!(duration_of(-1, 0).unwrap() < neg_half);
        assert_eq!(neg_half.checked_mul(2), duration_of(-1, 0));
        assert_eq!(Duration::of_seconds(3).checked_mul(-2), Some(Duration::of_seconds(-6)));
        assert_eq!(Duration::of_seconds(i64::MAX).checked_mul(2), None);
        assert_eq!(Duration::of_seconds(5).checked_mul(0), Some(Duration::ZERO));
        assert_eq!(
            std::cmp::max(Duration::of_seconds(1), neg_half),
            Duration::of_seconds(1)
        );
        assert_eq!(std::cmp::min(Duration::of_seconds(1), neg_half), neg_half);
    }
}

#[cfg(test)]
mod hardening_tests {
    use super::*;

    #[test]
    fn seeded_time_parsers_and_normalized_roundtrips() {
        let alphabet: Vec<char> = "0123456789PTDHMSZptdhmsz+-,.: ١９𝟙\0".chars().collect();
        let mut seed = 0x12_71ae_u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..10_000 {
            let len = next() as usize % 80;
            let input: String = (0..len).map(|_| alphabet[next() as usize % alphabet.len()]).collect();
            let _ = parse_instant(&input);
            let _ = parse_duration(&input);
            let seconds = next() as i64;
            let adjustment = next() as i64;
            if let Some(d) = duration_of(seconds, adjustment) {
                assert_eq!(parse_duration(&d.to_string()), Some(d));
            }
            if let Some(i) = instant_of(seconds, adjustment) {
                assert_eq!(parse_instant(&i.to_string()), Some(i));
            }
        }
        for seconds in [INSTANT_MIN_SECONDS, INSTANT_MAX_SECONDS, 0, -1] {
            for adjustment in [0, 1, 999_999_999, i64::MIN, i64::MAX] {
                if let Some(i) = instant_of(seconds, adjustment) {
                    assert_eq!(parse_instant(&i.to_string()), Some(i));
                }
            }
        }
    }
}
