use std::collections::BTreeMap;

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{AppError, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuotaWindow {
    pub limit_id: Option<String>,
    pub official_name: Option<String>,
    pub local_name: String,
    pub kind: String,
    pub used_percent: f32,
    pub remaining_percent: f32,
    pub window_duration_minutes: Option<i64>,
    pub resets_at: Option<DateTime<Utc>>,
    pub exhausted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenUsageSummary {
    pub lifetime_tokens: Option<i64>,
    pub current_streak_days: Option<i64>,
    pub longest_streak_days: Option<i64>,
    pub peak_daily_tokens: Option<i64>,
    pub longest_running_turn_seconds: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuotaSnapshot {
    pub queried_at: DateTime<Utc>,
    pub windows: Vec<QuotaWindow>,
    pub rate_limit_reached_type: Option<String>,
    pub spend_control_reached: Option<bool>,
    pub reset_credits_available: Option<i64>,
    pub token_usage: Option<TokenUsageSummary>,
}

pub fn remaining_percent(used: f64) -> f32 {
    (100.0 - used.clamp(0.0, 100.0)) as f32
}

pub fn parse_rate_limits(result: &Value, now: DateTime<Utc>) -> Result<QuotaSnapshot> {
    let mut snapshots = Vec::<(Option<String>, &Value)>::new();

    if let Some(by_id) = result
        .get("rateLimitsByLimitId")
        .and_then(Value::as_object)
        .filter(|map| !map.is_empty())
    {
        let ordered: BTreeMap<_, _> = by_id.iter().collect();
        for (limit_id, snapshot) in ordered {
            snapshots.push((Some(limit_id.clone()), snapshot));
        }
    } else if let Some(snapshot) = result.get("rateLimits") {
        snapshots.push((
            snapshot
                .get("limitId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            snapshot,
        ));
    } else {
        return Err(AppError::ProtocolIncompatible(
            "响应缺少 rateLimits 和 rateLimitsByLimitId".to_owned(),
        ));
    }

    let mut windows = Vec::new();
    let mut reached = None;
    let mut spend_control_reached = None;

    for (limit_id, snapshot) in snapshots {
        let official_name = snapshot
            .get("limitName")
            .and_then(Value::as_str)
            .map(str::to_owned);
        reached = reached.or_else(|| {
            snapshot
                .get("rateLimitReachedType")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        spend_control_reached = spend_control_reached
            .or_else(|| snapshot.get("spendControlReached").and_then(Value::as_bool));

        for kind in ["primary", "secondary"] {
            let Some(window) = snapshot.get(kind).filter(|value| !value.is_null()) else {
                continue;
            };
            let Some(used) = window.get("usedPercent").and_then(Value::as_f64) else {
                continue;
            };
            let duration = window
                .get("windowDurationMins")
                .and_then(Value::as_i64)
                .filter(|duration| *duration > 0);
            let resets_at = window
                .get("resetsAt")
                .and_then(Value::as_i64)
                .and_then(timestamp_from_unknown_unit);
            let used_clamped = used.clamp(0.0, 100.0) as f32;
            windows.push(QuotaWindow {
                limit_id: limit_id.clone(),
                official_name: official_name.clone(),
                local_name: window_name(
                    official_name.as_deref(),
                    limit_id.as_deref(),
                    duration,
                    kind,
                ),
                kind: kind.to_owned(),
                used_percent: used_clamped,
                remaining_percent: remaining_percent(used),
                window_duration_minutes: duration,
                resets_at,
                exhausted: used >= 100.0,
            });
        }
    }

    let reset_credits_available = result
        .get("rateLimitResetCredits")
        .and_then(|value| value.get("availableCount"))
        .and_then(Value::as_i64);

    Ok(QuotaSnapshot {
        queried_at: now,
        windows,
        rate_limit_reached_type: reached,
        spend_control_reached,
        reset_credits_available,
        token_usage: None,
    })
}

pub fn parse_token_usage(result: &Value) -> Option<TokenUsageSummary> {
    let summary = result.get("summary")?;
    Some(TokenUsageSummary {
        lifetime_tokens: summary.get("lifetimeTokens").and_then(Value::as_i64),
        current_streak_days: summary.get("currentStreakDays").and_then(Value::as_i64),
        longest_streak_days: summary.get("longestStreakDays").and_then(Value::as_i64),
        peak_daily_tokens: summary.get("peakDailyTokens").and_then(Value::as_i64),
        longest_running_turn_seconds: summary.get("longestRunningTurnSec").and_then(Value::as_i64),
    })
}

pub fn timestamp_from_unknown_unit(raw: i64) -> Option<DateTime<Utc>> {
    let seconds = if raw.abs() > 100_000_000_000_000 {
        raw / 1_000_000
    } else if raw.abs() > 100_000_000_000 {
        raw / 1_000
    } else {
        raw
    };
    Utc.timestamp_opt(seconds, 0).single()
}

fn window_name(
    official_name: Option<&str>,
    limit_id: Option<&str>,
    duration: Option<i64>,
    kind: &str,
) -> String {
    if let Some(name) = official_name.filter(|name| !name.trim().is_empty()) {
        return match duration {
            Some(duration) => format!("{name} · {}", duration_label(duration)),
            None => name.to_owned(),
        };
    }

    if let Some(duration) = duration {
        return duration_label(duration);
    }

    let bucket = if kind == "primary" {
        "主要额度窗口"
    } else {
        "次要额度窗口"
    };
    match limit_id.filter(|id| !id.is_empty()) {
        Some(id) => format!("{bucket} · {id}"),
        None => bucket.to_owned(),
    }
}

pub fn duration_label(minutes: i64) -> String {
    match minutes {
        60 => "1 小时".to_owned(),
        300 => "5 小时".to_owned(),
        1_440 => "每日".to_owned(),
        10_080 => "每周".to_owned(),
        minutes if minutes % 1_440 == 0 => format!("{} 天窗口", minutes / 1_440),
        minutes if minutes % 60 == 0 => format!("{} 小时窗口", minutes / 60),
        minutes => format!("{minutes} 分钟窗口"),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::{
        TokenUsageSummary, parse_rate_limits, remaining_percent, timestamp_from_unknown_unit,
    };

    #[test]
    fn clamps_invalid_percentages() {
        assert_eq!(remaining_percent(-2.5), 100.0);
        assert_eq!(remaining_percent(40.5), 59.5);
        assert_eq!(remaining_percent(120.0), 0.0);
    }

    #[test]
    fn parses_multi_bucket_and_unknown_fields() {
        let input = json!({
            "rateLimits": {"primary": {"usedPercent": 99}},
            "rateLimitsByLimitId": {
                "codex": {
                    "limitId": "codex",
                    "limitName": "Codex",
                    "primary": {
                        "usedPercent": 24.5,
                        "windowDurationMins": 300,
                        "resetsAt": 1_735_689_600,
                        "futureField": "ignored"
                    },
                    "secondary": {
                        "usedPercent": 101,
                        "windowDurationMins": 10080,
                        "resetsAt": null
                    },
                    "spendControlReached": false
                },
                "other": {
                    "primary": {"usedPercent": -4}
                }
            },
            "rateLimitResetCredits": {"availableCount": 2},
            "newTopLevelField": true
        });
        let now = Utc.with_ymd_and_hms(2026, 7, 30, 0, 0, 0).unwrap();
        let result = parse_rate_limits(&input, now).unwrap();
        assert_eq!(result.windows.len(), 3);
        assert_eq!(result.windows[0].remaining_percent, 75.5);
        assert!(result.windows[1].exhausted);
        assert_eq!(result.windows[2].used_percent, 0.0);
        assert_eq!(result.reset_credits_available, Some(2));
    }

    #[test]
    fn skips_missing_window_fields_and_handles_no_reset() {
        let input = json!({
            "rateLimits": {
                "primary": {"windowDurationMins": 300},
                "secondary": {"usedPercent": 10.0}
            }
        });
        let result = parse_rate_limits(&input, Utc::now()).unwrap();
        assert_eq!(result.windows.len(), 1);
        assert_eq!(result.windows[0].resets_at, None);
    }

    #[test]
    fn accepts_seconds_milliseconds_and_microseconds() {
        let expected = timestamp_from_unknown_unit(1_735_689_600);
        assert_eq!(expected, timestamp_from_unknown_unit(1_735_689_600_000));
        assert_eq!(expected, timestamp_from_unknown_unit(1_735_689_600_000_000));
    }

    #[test]
    fn token_usage_type_is_serializable() {
        let value = TokenUsageSummary {
            lifetime_tokens: Some(10),
            current_streak_days: None,
            longest_streak_days: None,
            peak_daily_tokens: None,
            longest_running_turn_seconds: None,
        };
        assert!(serde_json::to_string(&value).is_ok());
    }
}
