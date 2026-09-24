//! Calendar helpers on Unix seconds, in UTC.

use crate::{Error, Result};

const SECS_PER_DAY: i64 = 86_400;
const DAYS_PER_ERA: i64 = 146_097; // 400 Gregorian years
const YEARS_PER_ERA: i64 = 400;
const EPOCH_SHIFT_DAYS: i64 = 719_468; // days from 0000-03-01 to 1970-01-01

/// Parses `YYYY-MM-DD` to the Unix time of that day's midnight, UTC.
///
/// # Errors
///
/// [`Error::Date`] when the text is not a valid calendar date.
pub fn parse_date(text: &str) -> Result<i64> {
    let bad = || Error::Date(text.to_owned());
    let mut parts = text.trim().splitn(3, '-');
    let mut next = |len: usize| -> Result<i64> {
        let part = parts.next().ok_or_else(bad)?;
        if part.len() != len || !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        part.parse().map_err(|_| bad())
    };
    let (year, month, day) = (next(4)?, next(2)?, next(2)?);
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return Err(bad());
    }
    Ok(days_from_civil(year, month, day) * SECS_PER_DAY)
}

/// Day number since 1970-01-01 UTC for a Unix time.
pub fn unix_day(unix: i64) -> i64 {
    unix.div_euclid(SECS_PER_DAY)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Howard Hinnant's `days_from_civil`: proleptic Gregorian date to days since the Unix epoch.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(YEARS_PER_ERA);
    let year_of_era = y - era * YEARS_PER_ERA;
    let shifted_month = (month + 9) % 12; // March is 0
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * DAYS_PER_ERA + day_of_era - EPOCH_SHIFT_DAYS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_dates() {
        assert_eq!(parse_date("1970-01-01").unwrap(), 0);
        assert_eq!(parse_date("2026-09-24").unwrap(), 1_790_208_000);
        assert_eq!(parse_date("2024-02-29").unwrap(), 1_709_164_800);
    }

    #[test]
    fn rejects_bad_dates() {
        for text in [
            "2026-02-30",
            "2025-02-29",
            "2026-13-01",
            "26-01-01",
            "2026-1-01",
            "soon",
        ] {
            assert!(parse_date(text).is_err(), "{text}");
        }
    }

    #[test]
    fn day_numbers_floor_negative_times() {
        assert_eq!(unix_day(0), 0);
        assert_eq!(unix_day(86_399), 0);
        assert_eq!(unix_day(-1), -1);
    }
}
