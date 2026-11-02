//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/stdout/StdoutReporter.kt,
//! report/html/HtmlSummaryReporter.kt, report/junit/JUnitReporter.kt,
//! device/file/FileTransferExtensions.kt, execution/progress/PoolProgressAccumulator.kt,
//! and analytics/metrics/remote/graphite/QueryableGraphiteClient.kt (under the same package).

use crate::{
    jnum::double_to_string,
    jtime::{Instant, civil_from_days},
};

/// Explicit locales covered by the reference oracle. Default output uses US (D8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Locale {
    Root,
    Us,
    German,
    Russian,
}

impl Locale {
    pub fn from_language_tag(tag: &str) -> Option<Self> {
        match tag {
            "und" => Some(Self::Root),
            "en-US" => Some(Self::Us),
            "de-DE" => Some(Self::German),
            "ru-RU" => Some(Self::Russian),
            _ => None,
        }
    }
}

/// Default-locale progress text uses en-US under D8.
pub fn format_percent(percent: i32) -> String {
    format_percent_with_locale(percent, Locale::Us)
}

/// Default-locale file-transfer text uses en-US under D8.
pub fn format_fixed(value: f64, precision: usize) -> Option<String> {
    format_fixed_with_locale(value, precision, Locale::Us)
}

/// UTC dates use en-US under D8; pinned locales use `format_date_with_locale`.
pub fn format_date(epoch_millis: i64, pattern: DatePattern) -> String {
    format_date_with_locale(epoch_millis, pattern, Locale::Us)
}

/// Graphite's DateTimeFormatter accepts the UTC LocalDate range, narrower than Instant.
/// `None` corresponds to DateTimeException when conversion to a UTC date fails.
pub fn format_graphite(instant: Instant) -> Option<String> {
    let seconds = instant.seconds();
    let (year, month, day) = civil_from_days(seconds.div_euclid(86400));
    if !(-999_999_999..=999_999_999).contains(&year) {
        return None;
    }
    let year = if year <= 0 { 1 - year } else { year };
    let sign = if year > 9999 { "+" } else { "" };
    let tod = seconds.rem_euclid(86400);
    Some(format!(
        "{:02}:{:02}_{sign}{year:04}{month:02}{day:02}",
        tod / 3600,
        tod / 60 % 60
    ))
}

/// `String.format("%02d%%", percent)`; width includes the minus sign.
pub fn format_percent_with_locale(percent: i32, _locale: Locale) -> String {
    format!("{percent:02}%")
}

/// `String.format("%.2f", value)` and `"%.4f"`. Java rounds its decimal
/// representation half up, rather than Rust's binary-value ties-to-even rounding.
pub fn format_fixed_with_locale(value: f64, precision: usize, locale: Locale) -> Option<String> {
    if precision != 2 && precision != 4 {
        return None;
    }
    if !value.is_finite() {
        return Some(double_to_string(value));
    }
    let text = double_to_string(value.abs());
    let (mantissa, exponent) = text.split_once('E').unwrap_or((&text, "0"));
    let exponent: i32 = exponent.parse().ok()?;
    let point = mantissa.find('.').unwrap_or(mantissa.len()) as i32 + exponent;
    let digits: Vec<u8> = mantissa.bytes().filter(u8::is_ascii_digit).collect();
    let end = point + precision as i32;
    let mut rounded: Vec<u8> = (0..end.max(0))
        .map(|i| digits.get(i as usize).copied().unwrap_or(b'0'))
        .collect();
    let round_up = end >= 0 && digits.get(end as usize).is_some_and(|d| *d >= b'5');
    if round_up {
        let mut carry = true;
        for d in rounded.iter_mut().rev() {
            if *d == b'9' {
                *d = b'0';
            } else {
                *d += 1;
                carry = false;
                break;
            }
        }
        if carry {
            rounded.insert(0, b'1');
        }
    }
    while rounded.len() <= precision {
        rounded.insert(0, b'0');
    }
    let split = rounded.len() - precision;
    let separator = match locale {
        Locale::German | Locale::Russian => ',',
        _ => '.',
    };
    let mut out = String::new();
    if value.is_sign_negative() {
        out.push('-');
    }
    for (i, d) in rounded.iter().enumerate() {
        if i == split {
            out.push(separator);
        }
        out.push(char::from(*d));
    }
    Some(out)
}

/// Commons Lang's `formatDuration(ms, "H'H' mm'm' ss's'")`.
/// Negative inputs raise IllegalArgumentException in Java.
pub fn format_duration(millis: i64) -> Option<String> {
    if millis < 0 {
        return None;
    }
    Some(format_hms(millis))
}

fn format_hms(millis: i64) -> String {
    let seconds = millis / 1000;
    format!("{}H {:02}m {:02}s", seconds / 3600, seconds / 60 % 60, seconds % 60)
}

pub fn format_stdout_duration(millis: i64) -> String {
    if millis <= 0 {
        "0s".to_owned()
    } else {
        format_hms(millis)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatePattern {
    Html,
    Junit,
    Graphite,
}

/// UTC only, matching all three call sites. SimpleDateFormat uses a hybrid
/// Julian/Gregorian calendar (cutover 1582-10-15); DateTimeFormatter is proleptic.
pub fn format_date_with_locale(epoch_millis: i64, pattern: DatePattern, locale: Locale) -> String {
    let seconds = epoch_millis.div_euclid(1000);
    let days = seconds.div_euclid(86400);
    let tod = seconds.rem_euclid(86400);
    let (mut year, month, day) = if pattern != DatePattern::Graphite && days < -141427 {
        // Julian day number -> Julian civil date, with Euclidean divisions for BCE.
        let c = days + 2440588 + 32082;
        let d = (4 * c + 3).div_euclid(1461);
        let e = c - (1461 * d).div_euclid(4);
        let m = (5 * e + 2).div_euclid(153);
        (
            d - 4800 + m.div_euclid(10),
            m + 3 - 12 * m.div_euclid(10),
            e - (153 * m + 2).div_euclid(5) + 1,
        )
    } else {
        civil_from_days(days)
    };
    // yyyy is year-of-era, even for the proleptic DateTimeFormatter.
    if year <= 0 {
        year = 1 - year;
    }
    let (hour, minute, second) = (tod / 3600, tod / 60 % 60, tod % 60);
    match pattern {
        DatePattern::Graphite => {
            let sign = if year > 9999 { "+" } else { "" };
            format!("{hour:02}:{minute:02}_{sign}{year:04}{month:02}{day:02}")
        }
        DatePattern::Junit => format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}"),
        DatePattern::Html => {
            let months = match locale {
                Locale::Root | Locale::Us => [
                    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
                ],
                Locale::German => [
                    "Jan.", "Feb.", "März", "Apr.", "Mai", "Juni", "Juli", "Aug.", "Sept.", "Okt.", "Nov.", "Dez.",
                ],
                Locale::Russian => [
                    "янв.",
                    "февр.",
                    "мар.",
                    "апр.",
                    "мая",
                    "июн.",
                    "июл.",
                    "авг.",
                    "сент.",
                    "окт.",
                    "нояб.",
                    "дек.",
                ],
            };
            format!(
                "{hour:02}:{minute:02}:{second:02} UTC, {} {day} {year:04}",
                months[month as usize - 1]
            )
        }
    }
}
