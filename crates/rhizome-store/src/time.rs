//! Reading and writing the timestamps IRCv3 `server-time` uses.
//!
//! The format is a fixed-shape UTC ISO 8601 string, `2026-09-25T10:00:00.000Z`.
//! Parsing it needs no date library: it is arithmetic on a proleptic Gregorian
//! calendar, done here with the well-known days-from-civil algorithm, which
//! keeps the crate free of a dependency it would use for one function.

/// Parses `YYYY-MM-DDTHH:MM:SS[.fff…]Z` into milliseconds since the Unix
/// epoch. Returns `None` for anything else, including a valid instant written
/// with a numeric offset: the specification mandates UTC, and guessing at
/// other shapes would put messages at the wrong time.
///
/// Fractional seconds of any length are accepted; digits beyond milliseconds
/// are dropped rather than rounded.
pub fn parse_server_time(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    // The shortest valid form is "YYYY-MM-DDTHH:MM:SSZ".
    if b.len() < 20 {
        return None;
    }
    if b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }

    let num = |range: std::ops::Range<usize>| -> Option<i64> {
        let digits = &b[range];
        if digits.iter().all(u8::is_ascii_digit) {
            Some(digits.iter().fold(0i64, |n, d| n * 10 + i64::from(d - b'0')))
        } else {
            None
        }
    };
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);

    let mut i = 19;
    let mut millis = 0i64;
    if b[i] == b'.' {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return None; // a dot with nothing after it
        }
        // Read the first three digits as milliseconds, padding on the right.
        for (place, digit) in b[start..i].iter().take(3).enumerate() {
            millis += i64::from(digit - b'0') * [100, 10, 1][place];
        }
    }
    if i + 1 != b.len() || b[i] != b'Z' {
        return None;
    }

    if !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // A leap second (23:59:60) has no representation in Unix time; the usual
    // convention is to fold it into the second before.
    let second = second.min(59);

    let days = days_from_civil(year, month, day);
    Some((((days * 24 + hour) * 60 + minute) * 60 + second) * 1000 + millis)
}

/// Formats milliseconds since the Unix epoch as `YYYY-MM-DDTHH:MM:SS.fffZ`.
pub fn format_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let of_day = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        of_day / 3600,
        of_day % 3600 / 60,
        of_day % 60,
    )
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(year) => 29,
        _ => 28,
    }
}

/// Days from 1970-01-01 to the given date (negative before it).
///
/// Howard Hinnant's algorithm: shift the year to start in March so the leap
/// day is the last day of the year, then count whole 400-year eras.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let month_from_march = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The inverse of [`days_from_civil`].
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_is_zero() {
        assert_eq!(parse_server_time("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(parse_server_time("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn matches_known_unix_timestamps() {
        // Independently known values, so the algorithm is checked against
        // something other than itself.
        for (text, secs) in [
            ("2000-03-01T00:00:00Z", 951_868_800),
            ("2024-02-29T12:34:56Z", 1_709_210_096),
            ("2038-01-19T03:14:07Z", 2_147_483_647),
            ("2026-01-01T00:00:00Z", 1_767_225_600),
        ] {
            assert_eq!(parse_server_time(text), Some(secs * 1000), "{text}");
        }
    }

    #[test]
    fn dates_before_the_epoch_are_negative() {
        assert_eq!(parse_server_time("1969-12-31T23:59:59.000Z"), Some(-1000));
        assert_eq!(parse_server_time("1969-12-31T23:59:59.250Z"), Some(-750));
    }

    #[test]
    fn fractional_seconds_of_any_length_are_read_as_milliseconds() {
        let base = parse_server_time("2026-09-25T10:00:00Z").unwrap();
        assert_eq!(parse_server_time("2026-09-25T10:00:00.5Z"), Some(base + 500));
        assert_eq!(parse_server_time("2026-09-25T10:00:00.05Z"), Some(base + 50));
        assert_eq!(parse_server_time("2026-09-25T10:00:00.123Z"), Some(base + 123));
        // Extra precision is truncated, not rounded up into the next millisecond.
        assert_eq!(parse_server_time("2026-09-25T10:00:00.123999Z"), Some(base + 123));
    }

    #[test]
    fn a_leap_second_folds_into_the_previous_second() {
        let before = parse_server_time("2016-12-31T23:59:59Z").unwrap();
        assert_eq!(parse_server_time("2016-12-31T23:59:60Z"), Some(before));
    }

    #[test]
    fn leap_days_are_validated() {
        assert!(parse_server_time("2024-02-29T00:00:00Z").is_some());
        assert!(parse_server_time("2023-02-29T00:00:00Z").is_none());
        assert!(parse_server_time("1900-02-29T00:00:00Z").is_none(), "1900 was not a leap year");
        assert!(parse_server_time("2000-02-29T00:00:00Z").is_some(), "2000 was");
    }

    #[test]
    fn out_of_range_fields_are_rejected() {
        for bad in [
            "2026-13-01T00:00:00Z",
            "2026-00-10T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2026-09-25T24:00:00Z",
            "2026-09-25T10:60:00Z",
            "2026-09-25T10:00:61Z",
        ] {
            assert_eq!(parse_server_time(bad), None, "{bad}");
        }
    }

    #[test]
    fn anything_but_the_exact_shape_is_rejected() {
        for bad in [
            "",
            "garbage",
            "2026-09-25 10:00:00Z",
            "2026-09-25T10:00:00",
            "2026-09-25T10:00:00+03:00",
            "2026-09-25T10:00:00.Z",
            "2026-09-25T10:00:00.123",
            "2026-09-25T10:00:00.123Zjunk",
            "2026-09-25T1O:00:00Z",
            "２０２６-09-25T10:00:00Z",
        ] {
            assert_eq!(parse_server_time(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn format_and_parse_round_trip() {
        for ms in [
            0,
            1,
            -1,
            999,
            951_868_800_000,
            1_709_210_096_789,
            -62_135_596_800_000, // 0001-01-01
            4_102_444_800_000,   // 2100-01-01
            1_758_794_400_123,
        ] {
            let text = format_ms(ms);
            assert_eq!(parse_server_time(&text), Some(ms), "{ms} -> {text}");
        }
    }

    #[test]
    fn every_day_over_a_long_span_round_trips() {
        // Catches an off-by-one in any month or leap-year boundary.
        let mut day = -800_000i64; // roughly year -220
        while day < 800_000 {
            let (y, m, d) = civil_from_days(day);
            assert_eq!(days_from_civil(y, m, d), day, "day {day} -> {y}-{m}-{d}");
            day += 1;
        }
    }

    #[test]
    fn format_pads_and_uses_utc() {
        assert_eq!(format_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_ms(1_758_794_400_123), "2025-09-25T10:00:00.123Z");
    }
}
