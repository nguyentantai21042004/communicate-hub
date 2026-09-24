//! `captain report --text`: the phase 1 report as plain text for a terminal.

use std::fmt::Write;

use serde_json::Value;

const SECS_PER_MINUTE: f64 = 60.0;
const SECS_PER_HOUR: f64 = 3600.0;
const PERCENT: f64 = 100.0;

/// Renders the report JSON. Unknown or missing fields print as `-`, so an older daemon's
/// report still renders.
pub fn render(report: &Value) -> String {
    let mut out = String::new();
    let days = report["days"].as_f64().unwrap_or_default();
    let _ = writeln!(out, "Phase 1 report, last {days:.1} days\n");

    let _ = writeln!(out, "Decision rules");
    for rule in array(&report["rules"]) {
        // A rule that holds has nothing to do; say what it watches instead.
        let detail = match rule["state"].as_str() {
            Some("fires" | "suggestion") => text(&rule["then"]).to_owned(),
            _ => format!("watching for {}", text(&rule["when"])),
        };
        let _ = writeln!(
            out,
            "  [{:<15}] {:<18} {detail}",
            text(&rule["state"]),
            text(&rule["id"]),
        );
    }

    for section in array(&report["sections"]) {
        let _ = writeln!(
            out,
            "\n{} {}",
            text(&section["question"]),
            text(&section["title"])
        );
        for metric in array(&section["metrics"]) {
            let stats: Vec<String> = array(&metric["summary"]).iter().map(stat).collect();
            let _ = writeln!(
                out,
                "  {:<4} {:<34} {}  (n={})",
                text(&metric["id"]),
                text(&metric["name"]),
                stats.join(", "),
                metric["n"].as_u64().unwrap_or_default(),
            );
        }
    }

    let candidates = array(&report["candidates"]);
    if !candidates.is_empty() {
        let _ = writeln!(out, "\nOut-of-scope chats that would have triggered");
        for c in candidates {
            let _ = writeln!(
                out,
                "  {:>16}  {:<30} {} of {} messages",
                c["chat"].as_i64().unwrap_or_default(),
                text(&c["title"]),
                c["would_trigger"].as_u64().unwrap_or_default(),
                c["messages"].as_u64().unwrap_or_default(),
            );
        }
    }
    out
}

fn array(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("-")
}

fn stat(s: &Value) -> String {
    let label = text(&s["label"]);
    let Some(v) = s["value"].as_f64() else {
        return format!("{label} -");
    };
    let value = match s["unit"].as_str() {
        Some("ratio") => format!("{:.0}%", v * PERCENT),
        Some("seconds") => duration(v),
        Some("per_day") => format!("{v:.1}/day"),
        Some("megabytes") => format!("{v:.1} MB"),
        Some("megabytes_per_day") => format!("{v:.2} MB/day"),
        _ => format!("{v:.0}"),
    };
    format!("{label} {value}")
}

fn duration(secs: f64) -> String {
    if secs >= SECS_PER_HOUR {
        format!("{:.1}h", secs / SECS_PER_HOUR)
    } else if secs >= SECS_PER_MINUTE {
        format!("{:.1}m", secs / SECS_PER_MINUTE)
    } else {
        format!("{secs:.0}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_stats_by_unit() {
        let report = json!({
            "days": 14.0,
            "rules": [{ "id": "volume_high", "state": "holds", "when": "many", "then": "cut" }],
            "sections": [{ "question": "Q1", "title": "Volume", "metrics": [{
                "id": "M1", "name": "Tickets per day", "n": 3,
                "summary": [
                    { "label": "p50", "value": 90.0, "unit": "seconds" },
                    { "label": "share", "value": 0.25, "unit": "ratio" },
                    { "label": "none", "value": null, "unit": "count" }
                ]
            }]}],
            "candidates": []
        });
        let text = render(&report);
        assert!(text.contains("p50 1.5m, share 25%, none -"));
        assert!(text.contains("[holds          ] volume_high        watching for many"));
    }
}
