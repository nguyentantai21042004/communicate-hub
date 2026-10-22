//! Query and body parsing. Bad input is a 400, and oversized limits are clamped.

use std::collections::HashMap;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::limits::{
    DEFAULT_DAYS, DEFAULT_EVENT_LIMIT, DEFAULT_HISTORY_SCAN, DEFAULT_PAGE_LIMIT, MAX_DAYS,
    MAX_EVENT_LIMIT, MAX_HISTORY_SCAN, MAX_OFFSET, MAX_PAGE_LIMIT, MIN_DAYS,
};
use crate::model::{
    AnalyticsQuery, ConversationKindFilter, ConversationQuery, DaysQuery, HistoryRequest,
    OverviewQuery, SourceFilter,
};

pub(crate) fn overview(raw: Option<&str>) -> Result<OverviewQuery> {
    let query = pairs(raw)?;
    Ok(OverviewQuery {
        days: days_or(&query, DEFAULT_DAYS)?,
        source: source_or(&query)?,
    })
}

pub(crate) fn conversations(raw: Option<&str>) -> Result<ConversationQuery> {
    let query = pairs(raw)?;
    let q = optional(&query, "q")
        .map(str::trim)
        .filter(|text| !text.is_empty());
    Ok(ConversationQuery {
        source: source_or(&query)?,
        kind: kind_or(&query)?,
        days: optional_days(&query)?,
        q: q.map(str::to_owned),
        limit: clamp_u32(&query, "limit", DEFAULT_PAGE_LIMIT, 1, MAX_PAGE_LIMIT)?,
        offset: clamp_u32(&query, "offset", 0, 0, MAX_OFFSET)?,
    })
}

pub(crate) fn analytics(raw: Option<&str>) -> Result<AnalyticsQuery> {
    let query = pairs(raw)?;
    Ok(AnalyticsQuery {
        days: days_or(&query, DEFAULT_DAYS)?,
        source: source_or(&query)?,
    })
}

pub(crate) fn days(raw: Option<&str>) -> Result<DaysQuery> {
    let query = pairs(raw)?;
    Ok(DaysQuery {
        days: days_or(&query, DEFAULT_DAYS)?,
    })
}

pub(crate) fn events_limit(raw: Option<&str>) -> Result<u32> {
    let query = pairs(raw)?;
    clamp_u32(&query, "limit", DEFAULT_EVENT_LIMIT, 1, MAX_EVENT_LIMIT)
}

pub(crate) fn history_body(bytes: &[u8]) -> Result<HistoryRequest> {
    let body: HistoryBody = serde_json::from_slice(bytes)
        .map_err(|error| Error::BadRequest(format!("history body: {error}")))?;
    if body.regex.is_empty() {
        return Err(Error::BadRequest("regex is empty".into()));
    }
    let scan = body.scan_limit.unwrap_or(DEFAULT_HISTORY_SCAN).max(1);
    Ok(HistoryRequest {
        chat: body.chat,
        regex: body.regex,
        scan_limit: usize::try_from(scan)
            .unwrap_or(usize::MAX)
            .min(MAX_HISTORY_SCAN),
        text: body.text,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryBody {
    chat: i64,
    regex: String,
    #[serde(default)]
    scan_limit: Option<u32>,
    #[serde(default)]
    text: bool,
}

fn pairs(raw: Option<&str>) -> Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    let Some(raw) = raw.filter(|text| !text.is_empty()) else {
        return Ok(out);
    };
    for part in raw.split('&') {
        if part.is_empty() {
            continue;
        }
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        let key = percent_decode(key)?;
        let value = percent_decode(value)?;
        out.insert(key, value);
    }
    Ok(out)
}

fn percent_decode(input: &str) -> Result<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => out.push(b' '),
            b'%' => {
                let Some(hex) = bytes.get(index + 1..index + 3) else {
                    return Err(Error::BadRequest("query encoding is invalid".into()));
                };
                let Some(text) = std::str::from_utf8(hex).ok() else {
                    return Err(Error::BadRequest("query encoding is invalid".into()));
                };
                let Some(byte) = u8::from_str_radix(text, 16).ok() else {
                    return Err(Error::BadRequest("query encoding is invalid".into()));
                };
                out.push(byte);
                index += 2;
            }
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8(out).map_err(|_| Error::BadRequest("query is not utf-8".into()))
}

fn optional<'a>(query: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    query
        .get(key)
        .map(String::as_str)
        .filter(|text| !text.is_empty())
}

fn source_or(query: &HashMap<String, String>) -> Result<SourceFilter> {
    match optional(query, "source") {
        None | Some("all") => Ok(SourceFilter::All),
        Some("telegram") => Ok(SourceFilter::Telegram),
        Some("zalo") => Ok(SourceFilter::Zalo),
        Some(other) => Err(Error::BadRequest(format!(
            "source must be all, telegram, or zalo, not {other}"
        ))),
    }
}

fn kind_or(query: &HashMap<String, String>) -> Result<Option<ConversationKindFilter>> {
    match optional(query, "kind") {
        None => Ok(None),
        Some("dm") => Ok(Some(ConversationKindFilter::Dm)),
        Some("group") => Ok(Some(ConversationKindFilter::Group)),
        Some("channel") => Ok(Some(ConversationKindFilter::Channel)),
        Some("forum") => Ok(Some(ConversationKindFilter::Forum)),
        Some(other) => Err(Error::BadRequest(format!(
            "kind must be dm, group, channel, or forum, not {other}"
        ))),
    }
}

fn days_or(query: &HashMap<String, String>, default: u32) -> Result<u32> {
    match optional(query, "days") {
        None => Ok(default),
        Some(text) => parse_days(text),
    }
}

fn optional_days(query: &HashMap<String, String>) -> Result<Option<u32>> {
    optional(query, "days").map(parse_days).transpose()
}

fn parse_days(text: &str) -> Result<u32> {
    let parsed = parse_i64(text, "days")?;
    if parsed < i64::from(MIN_DAYS) {
        return Err(Error::BadRequest("days must be at least 1".into()));
    }
    Ok(u32::try_from(parsed).unwrap_or(u32::MAX).min(MAX_DAYS))
}

fn clamp_u32(
    query: &HashMap<String, String>,
    key: &str,
    default: u32,
    min: u32,
    max: u32,
) -> Result<u32> {
    let Some(text) = optional(query, key) else {
        return Ok(default);
    };
    let parsed = parse_i64(text, key)?;
    if parsed < i64::from(min) {
        return Err(Error::BadRequest(format!("{key} must be at least {min}")));
    }
    Ok(u32::try_from(parsed).unwrap_or(u32::MAX).min(max))
}

fn parse_i64(text: &str, key: &str) -> Result<i64> {
    text.parse::<i64>()
        .map_err(|_| Error::BadRequest(format!("{key} must be an integer")))
}
