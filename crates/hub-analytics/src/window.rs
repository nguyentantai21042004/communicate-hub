//! Half-open rolling windows and UTC calendar days. Callers pass `now`.

use crate::limits::{SECS_PER_DAY, SECS_PER_HOUR, WEEKDAY_SHIFT};
use crate::{Error, Result};

pub(crate) fn rolling_start(now_unix_secs: i64, days: i64) -> Result<i64> {
    if days <= 0 {
        return Err(Error::InvalidRange);
    }
    let span = days.checked_mul(SECS_PER_DAY).ok_or(Error::InvalidRange)?;
    now_unix_secs.checked_sub(span).ok_or(Error::InvalidRange)
}

/// Inclusive UTC day numbers covering the last `days` days, ending on the day of `now`.
pub(crate) fn calendar_days(now_unix_secs: i64, days: i64) -> Result<(i64, i64)> {
    if days <= 0 {
        return Err(Error::InvalidRange);
    }
    let last = now_unix_secs.div_euclid(SECS_PER_DAY);
    let back = days.checked_sub(1).ok_or(Error::InvalidRange)?;
    let first = last.checked_sub(back).ok_or(Error::InvalidRange)?;
    Ok((first, last))
}

pub(crate) fn hour_index(unix_secs: i64) -> i64 {
    unix_secs.div_euclid(SECS_PER_HOUR)
}

pub(crate) fn bucket_start(day: i64, hour: u8) -> Result<i64> {
    let day_secs = day.checked_mul(SECS_PER_DAY).ok_or(Error::InvalidRange)?;
    let hour_secs = i64::from(hour)
        .checked_mul(SECS_PER_HOUR)
        .ok_or(Error::InvalidRange)?;
    day_secs.checked_add(hour_secs).ok_or(Error::InvalidRange)
}

/// Monday is 0. Unix day 0 (1970-01-01) is Thursday, which is 3 under that numbering.
pub(crate) fn weekday_monday0(unix_day: i64) -> Result<u8> {
    let shifted = unix_day
        .checked_add(WEEKDAY_SHIFT)
        .ok_or(Error::InvalidRange)?;
    let weekday = shifted.rem_euclid(7);
    u8::try_from(weekday).map_err(|_| Error::InvalidRange)
}

pub(crate) fn local_parts(bucket_start: i64, offset_minutes: i32) -> Result<(u8, u8)> {
    let shift = i64::from(offset_minutes)
        .checked_mul(crate::limits::SECS_PER_MINUTE)
        .ok_or(Error::InvalidRange)?;
    let local = bucket_start.checked_add(shift).ok_or(Error::InvalidRange)?;
    let day = local.div_euclid(SECS_PER_DAY);
    let hour = local.div_euclid(SECS_PER_HOUR).rem_euclid(24);
    let hour = u8::try_from(hour).map_err(|_| Error::InvalidRange)?;
    Ok((weekday_monday0(day)?, hour))
}

pub(crate) fn add(total: &mut u64, extra: u64) -> Result<()> {
    *total = total
        .checked_add(extra)
        .ok_or_else(|| Error::Corrupt("count overflow".to_owned()))?;
    Ok(())
}

#[expect(
    clippy::cast_precision_loss,
    reason = "reply seconds stay far below 2^52"
)]
pub(crate) fn minutes(secs: i64) -> f64 {
    secs as f64 / crate::limits::SECS_PER_MINUTE as f64
}
