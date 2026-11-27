//! Разбор ответов Twitch: архив прошлых эфиров (видео) и расписание стримера.

use chrono::DateTime;
use serde_json::Value;

use crate::prediction::ScheduleSegment;

pub const SCHEDULE_HORIZON_DAYS: f64 = 14.0;

/// ISO-время Twitch (`2026-10-10T18:00:00Z`) -> unix time.
pub fn iso_to_ts(text: &str) -> Option<f64> {
    let dt = DateTime::parse_from_rfc3339(text).ok()?;
    Some(dt.timestamp() as f64 + dt.timestamp_subsec_millis() as f64 / 1000.0)
}

/// `3h12m45s` -> секунды. Непонятное значение даёт 0.
pub fn parse_twitch_duration(text: &str) -> u64 {
    let mut total = 0u64;
    let mut number = String::new();
    let mut last_rank = 0;
    for c in text.chars() {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        let (rank, mult) = match c {
            'h' => (1, 3600),
            'm' => (2, 60),
            's' => (3, 1),
            _ => return 0,
        };
        if number.is_empty() || rank <= last_rank {
            return 0;
        }
        total += number.parse::<u64>().unwrap_or(0) * mult;
        number.clear();
        last_rank = rank;
    }
    if number.is_empty() {
        total
    } else {
        0
    }
}

/// Прошлый эфир из архива Twitch.
#[derive(Debug, Clone, PartialEq)]
pub struct PastStream {
    pub stream_id: String,
    pub login: String,
    pub started_ts: f64,
    pub ended_ts: f64,
    pub title: String,
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Записи прошлых эфиров из списка видео (`type=archive`); битые пропускаются.
pub fn videos_to_streams(videos: &[Value]) -> Vec<PastStream> {
    let mut rows = Vec::new();
    for v in videos {
        if let Some(kind) = v.get("type").and_then(Value::as_str) {
            if kind != "archive" {
                continue;
            }
        }
        let login = text(v, "user_login").to_lowercase();
        let Some(started) = iso_to_ts(text(v, "created_at")) else { continue };
        if login.is_empty() {
            continue;
        }
        let stream_id = match text(v, "stream_id") {
            "" => format!("vod:{}", text(v, "id")),
            id => id.to_string(),
        };
        rows.push(PastStream {
            stream_id,
            login,
            started_ts: started,
            ended_ts: started + parse_twitch_duration(text(v, "duration")) as f64,
            title: text(v, "title").to_string(),
        });
    }
    rows
}

/// Ответ Get Channel Stream Schedule -> слоты на ближайшие 14 дней.
pub fn parse_schedule(body: &Value, now: f64) -> Vec<ScheduleSegment> {
    let data = body.get("data").unwrap_or(&Value::Null);
    let vacation = data.get("vacation").filter(|v| v.is_object());
    let vac_start = vacation.and_then(|v| iso_to_ts(text(v, "start_time")));
    let vac_end = vacation.and_then(|v| iso_to_ts(text(v, "end_time")));

    let Some(segments) = data.get("segments").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for seg in segments {
        let Some(start) = iso_to_ts(text(seg, "start_time")) else { continue };
        if start > now + SCHEDULE_HORIZON_DAYS * 86400.0 {
            continue;
        }
        let on_vacation = matches!((vac_start, vac_end), (Some(a), Some(b)) if a <= start && start <= b);
        let cancelled = seg
            .get("canceled_until")
            .map(|v| !v.is_null() && v.as_str().map_or(true, |s| !s.is_empty()))
            .unwrap_or(false);
        out.push(ScheduleSegment {
            start_ts: start,
            end_ts: iso_to_ts(text(seg, "end_time")),
            title: text(seg, "title").to_string(),
            category: seg
                .get("category")
                .and_then(|c| c.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            canceled: cancelled || on_vacation,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn duration() {
        assert_eq!(parse_twitch_duration("3h12m45s"), 3 * 3600 + 12 * 60 + 45);
        assert_eq!(parse_twitch_duration("45m"), 2700);
        assert_eq!(parse_twitch_duration("30s"), 30);
        assert_eq!(parse_twitch_duration(""), 0);
        assert_eq!(parse_twitch_duration("мусор"), 0);
        assert_eq!(parse_twitch_duration("5m2h"), 0); // порядок единиц важен
        assert_eq!(parse_twitch_duration("12"), 0);
    }

    #[test]
    fn videos() {
        let videos = vec![
            json!({"id": "1", "stream_id": "555", "user_login": "Amy", "created_at": "2026-10-10T18:00:00Z", "duration": "2h", "title": "T", "type": "archive"}),
            json!({"id": "2", "stream_id": null, "user_login": "amy", "created_at": "2026-10-09T18:00:00Z", "duration": "1h30m", "type": "archive"}),
            json!({"id": "3", "user_login": "amy", "created_at": "2026-10-08T18:00:00Z", "type": "highlight"}),
            json!({"id": "4", "user_login": "amy", "created_at": "не дата", "type": "archive"}),
            json!({"id": "5", "user_login": "", "created_at": "2026-10-08T18:00:00Z"}),
        ];
        let rows = videos_to_streams(&videos);
        let ids: Vec<&str> = rows.iter().map(|r| r.stream_id.as_str()).collect();
        assert_eq!(ids, ["555", "vod:2"]);
        assert_eq!(rows[0].login, "amy");
        assert_eq!(rows[0].ended_ts - rows[0].started_ts, 7200.0);
    }

    #[test]
    fn schedule() {
        let now = iso_to_ts("2026-10-14T00:00:00Z").unwrap();
        let body = json!({"data": {
            "segments": [
                {"start_time": "2026-10-15T18:00:00Z", "end_time": "2026-10-15T21:00:00Z", "title": "A", "canceled_until": null, "category": {"name": "Chess"}},
                {"start_time": "2026-10-16T18:00:00Z", "title": "B", "canceled_until": "2026-10-17T00:00:00Z", "category": null},
                {"start_time": "2026-10-18T18:00:00Z", "title": "C"},
                {"start_time": "2026-12-01T18:00:00Z", "title": "далеко"}
            ],
            "vacation": {"start_time": "2026-10-18T00:00:00Z", "end_time": "2026-10-20T00:00:00Z"}
        }});
        let segs = parse_schedule(&body, now);
        let titles: Vec<&str> = segs.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["A", "B", "C"]);
        assert_eq!(segs.iter().map(|s| s.canceled).collect::<Vec<_>>(), [false, true, true]);
        assert_eq!(segs[0].category, "Chess");
        assert!(parse_schedule(&Value::Null, now).is_empty());
        assert!(parse_schedule(&json!({"data": {"segments": null}}), now).is_empty());
    }
}
