//! Прогноз следующего эфира: по расписанию Twitch и по привычкам стримера (история начал эфира).

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Timelike};
use serde::{Deserialize, Serialize};

pub const WINDOW_DAYS: i64 = 56; // смотрим последние 8 недель
pub const MIN_DAYS_WITH_STREAMS: usize = 3; // меньше эфиров прогнозировать нечестно
pub const OVERDUE_SECONDS: f64 = 90.0 * 60.0; // опоздание дольше 1,5 часа: сегодня эфира, видимо, не будет
pub const HORIZON_DAYS: i64 = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    High,
    Medium,
    Low,
    Schedule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Habit,
    Schedule,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Prediction {
    /// Ожидаемое начало эфира (unix time).
    pub start_ts: f64,
    pub level: Level,
    /// 0..1, уверенность по привычкам.
    pub score: f64,
    /// Сколько недель с таким днём недели канал стримил.
    pub weeks_with: u32,
    /// Сколько таких дней было в окне.
    pub weeks_total: u32,
    /// Типичный разброс времени начала, минуты.
    pub spread_minutes: i64,
    pub source: Source,
    /// Название из расписания Twitch.
    pub title: String,
}

/// Слот расписания Twitch. Формат хранения совпадает с Python-версией.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScheduleSegment {
    pub start_ts: f64,
    #[serde(default)]
    pub end_ts: Option<f64>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub canceled: bool,
}

fn median(values: &[f64]) -> f64 {
    let mut ordered = values.to_vec();
    ordered.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mid = ordered.len() / 2;
    if ordered.len() % 2 == 1 {
        ordered[mid]
    } else {
        (ordered[mid - 1] + ordered[mid]) / 2.0
    }
}

/// Округление «к чётному», как в Python, чтобы результаты совпадали с прежней версией.
fn round_half_even(x: f64) -> i64 {
    let floor = x.floor();
    if (x - floor - 0.5).abs() < 1e-9 {
        let f = floor as i64;
        if f % 2 == 0 {
            f
        } else {
            f + 1
        }
    } else {
        x.round() as i64
    }
}

/// (типичная минута суток, разброс). Начала около полуночи (23:50 и 00:10) считаются вместе.
pub fn typical_minute(minutes: &[i64]) -> (i64, i64) {
    let mut values: Vec<f64> = minutes.iter().map(|&m| m as f64).collect();
    if values.iter().any(|&m| m >= 1080.0) && values.iter().any(|&m| m < 360.0) {
        for v in values.iter_mut() {
            if *v < 360.0 {
                *v += 1440.0;
            }
        }
    }
    let med = median(&values);
    let spread = median(&values.iter().map(|v| (v - med).abs()).collect::<Vec<_>>());
    (round_half_even(med).rem_euclid(1440), round_half_even(spread))
}

fn level_for(score: f64) -> Level {
    if score >= 0.7 {
        Level::High
    } else if score >= 0.4 {
        Level::Medium
    } else {
        Level::Low
    }
}

fn at<Tz: TimeZone>(tz: &Tz, ts: f64) -> Option<DateTime<Tz>> {
    let secs = ts.floor() as i64;
    let nanos = ((ts - ts.floor()) * 1e9) as u32;
    tz.timestamp_opt(secs, nanos).single()
}

fn from_schedule(schedule: &[ScheduleSegment], now: f64, live: bool) -> Option<Prediction> {
    let mut segments: Vec<&ScheduleSegment> = schedule.iter().collect();
    segments.sort_by(|a, b| a.start_ts.partial_cmp(&b.start_ts).unwrap());
    for seg in segments {
        if seg.canceled {
            continue;
        }
        if seg.start_ts < now - 1800.0 || (live && seg.start_ts <= now) {
            continue;
        }
        if seg.start_ts > now + 7.0 * 86400.0 {
            break;
        }
        return Some(Prediction {
            start_ts: seg.start_ts,
            level: Level::Schedule,
            score: 1.0,
            weeks_with: 0,
            weeks_total: 0,
            spread_minutes: 0,
            source: Source::Schedule,
            title: seg.title.clone(),
        });
    }
    None
}

fn from_habits<Tz: TimeZone>(starts: &[f64], now: f64, live: bool, tz: &Tz) -> Option<Prediction> {
    let window_start = now - WINDOW_DAYS as f64 * 86400.0;
    let mut recent: Vec<f64> = starts.iter().copied().filter(|t| *t >= window_start && *t <= now).collect();
    recent.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let mut first_start_of_day: BTreeMap<NaiveDate, i64> = BTreeMap::new();
    for ts in recent {
        if let Some(dt) = at(tz, ts) {
            first_start_of_day.entry(dt.date_naive()).or_insert(dt.hour() as i64 * 60 + dt.minute() as i64);
        }
    }
    if first_start_of_day.len() < MIN_DAYS_WITH_STREAMS {
        return None;
    }

    let today = at(tz, now)?.date_naive();
    let first_day = at(tz, window_start)?.date_naive();

    // сколько раз в окне встретился каждый день недели (сегодня считаем, только если эфир уже был)
    let mut occurrences = [0u32; 7];
    let mut minutes_by_weekday: Vec<Vec<i64>> = vec![Vec::new(); 7];
    let mut day = first_day;
    while day <= today {
        let w = day.weekday().num_days_from_monday() as usize;
        if let Some(&minute) = first_start_of_day.get(&day) {
            occurrences[w] += 1;
            minutes_by_weekday[w].push(minute);
        } else if day < today {
            occurrences[w] += 1;
        }
        day += Duration::days(1);
    }

    let streamed_today = first_start_of_day.contains_key(&today);
    for offset in 0..HORIZON_DAYS {
        let day = today + Duration::days(offset);
        let w = day.weekday().num_days_from_monday() as usize;
        let k = minutes_by_weekday[w].len() as u32;
        let n = occurrences[w];
        if k < 2 || n == 0 || (k as f64) / (n as f64) < 0.25 {
            continue;
        }
        if offset == 0 && (streamed_today || live) {
            continue;
        }
        let (minute, spread) = typical_minute(&minutes_by_weekday[w]);
        let midnight = tz.from_local_datetime(&day.and_hms_opt(0, 0, 0)?).earliest()?;
        let start_ts = (midnight + Duration::minutes(minute)).timestamp() as f64;
        if start_ts < now - OVERDUE_SECONDS {
            continue;
        }

        let mut score = (k as f64 / n as f64) * (1.0 - (spread.min(120) as f64) / 240.0);
        // три последних такого дня недели подряд без эфира: канал, похоже, сменил режим
        let mut recent_days: Vec<bool> = Vec::new();
        let mut probe = today - Duration::days(1);
        while recent_days.len() < 3 && probe >= first_day {
            if probe.weekday().num_days_from_monday() as usize == w {
                recent_days.push(first_start_of_day.contains_key(&probe));
            }
            probe -= Duration::days(1);
        }
        if recent_days.len() == 3 && !recent_days.iter().any(|&b| b) {
            score *= 0.5;
        }
        return Some(Prediction {
            start_ts,
            level: level_for(score),
            score: (score * 1000.0).round() / 1000.0,
            weeks_with: k,
            weeks_total: n,
            spread_minutes: spread,
            source: Source::Habit,
            title: String::new(),
        });
    }
    None
}

/// Ожидаемое начало следующего эфира или `None`, если данных мало.
///
/// `starts`: unix-время начал прошлых эфиров; `schedule`: слоты расписания Twitch;
/// `tz`: часовой пояс, в котором считаем «день недели» и «время суток».
pub fn predict_next<Tz: TimeZone>(
    starts: &[f64],
    now: f64,
    schedule: Option<&[ScheduleSegment]>,
    live: bool,
    tz: &Tz,
) -> Option<Prediction> {
    if let Some(found) = schedule.filter(|s| !s.is_empty()).and_then(|s| from_schedule(s, now, live)) {
        return Some(found);
    }
    from_habits(starts, now, live, tz)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn ts(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> f64 {
        Utc.with_ymd_and_hms(y, m, d, hh, mm, 0).unwrap().timestamp() as f64
    }

    /// 8 суббот подряд в 19:00 UTC, последняя за неделю до 2026-10-17.
    fn saturdays() -> Vec<f64> {
        (1..=8).map(|i| ts(2026, 10, 17, 0, 0) - 7.0 * 86400.0 * i as f64 + 19.0 * 3600.0).collect()
    }

    fn wednesday_noon() -> f64 {
        ts(2026, 10, 14, 12, 0)
    }

    fn predict(starts: &[f64], now: f64) -> Option<Prediction> {
        predict_next(starts, now, None, false, &Utc)
    }

    #[test]
    fn weekly_pattern_predicts_next_saturday_with_high_level() {
        let p = predict(&saturdays(), wednesday_noon()).unwrap();
        assert_eq!(p.start_ts, ts(2026, 10, 17, 19, 0));
        assert_eq!(p.level, Level::High);
        assert_eq!((p.weeks_with, p.weeks_total), (8, 8));
        assert_eq!(p.source, Source::Habit);
    }

    #[test]
    fn too_little_history() {
        assert!(predict(&saturdays()[..2], wednesday_noon()).is_none());
        assert!(predict(&[], wednesday_noon()).is_none());
    }

    #[test]
    fn today_slot_still_ahead() {
        let p = predict(&saturdays(), ts(2026, 10, 17, 17, 0)).unwrap();
        assert_eq!(p.start_ts, ts(2026, 10, 17, 19, 0));
    }

    #[test]
    fn slot_missed_by_far_moves_to_next_week() {
        let p = predict(&saturdays(), ts(2026, 10, 17, 23, 0)).unwrap();
        assert_eq!(p.start_ts, ts(2026, 10, 24, 19, 0));
    }

    #[test]
    fn slightly_late_is_still_expected() {
        let now = ts(2026, 10, 17, 19, 30);
        let p = predict(&saturdays(), now).unwrap();
        assert_eq!(p.start_ts, ts(2026, 10, 17, 19, 0));
        assert!(p.start_ts < now);
    }

    #[test]
    fn already_streamed_today_or_live_skips_today() {
        let mut with_today = saturdays();
        with_today.push(ts(2026, 10, 17, 19, 5));
        assert_eq!(predict(&with_today, ts(2026, 10, 17, 20, 0)).unwrap().start_ts, ts(2026, 10, 24, 19, 0));
        let live = predict_next(&saturdays(), ts(2026, 10, 17, 19, 20), None, true, &Utc).unwrap();
        assert_eq!(live.start_ts, ts(2026, 10, 24, 19, 0));
    }

    #[test]
    fn irregular_streamer_gets_low_level_or_nothing() {
        let days = [
            ts(2026, 10, 13, 15, 0),
            ts(2026, 10, 6, 20, 0),
            ts(2026, 9, 25, 12, 0),
            ts(2026, 9, 18, 18, 0),
            ts(2026, 9, 29, 22, 0),
        ];
        let p = predict(&days, wednesday_noon());
        assert!(p.is_none() || p.unwrap().level == Level::Low);
    }

    #[test]
    fn three_skipped_weeks_in_a_row_lower_confidence() {
        let old_pattern = saturdays()[3..].to_vec(); // эфиры были 4-8 недель назад, три последние субботы пропущены
        let p = predict(&old_pattern, wednesday_noon()).unwrap();
        let full = predict(&saturdays(), wednesday_noon()).unwrap();
        assert!(p.score < full.score);
        assert_ne!(p.level, Level::High);
    }

    #[test]
    fn midnight_wrap_is_handled() {
        let (minute, spread) = typical_minute(&[23 * 60 + 50, 10, 23 * 60 + 55, 5]);
        assert!(minute >= 23 * 60 || minute <= 30, "{minute}");
        assert!(spread < 30);
    }

    #[test]
    fn schedule_beats_habits_and_skips_canceled_and_old() {
        let now = wednesday_noon();
        let seg = |start: f64, canceled: bool, title: &str| ScheduleSegment {
            start_ts: start,
            canceled,
            title: title.into(),
            ..Default::default()
        };
        let schedule = vec![
            seg(now - 7200.0, false, "прошлое"),
            seg(ts(2026, 10, 15, 18, 0), true, "отменено"),
            seg(ts(2026, 10, 16, 18, 0), false, "Пятничный стрим"),
        ];
        let p = predict_next(&saturdays(), now, Some(&schedule), false, &Utc).unwrap();
        assert_eq!((p.source, p.level, p.start_ts), (Source::Schedule, Level::Schedule, ts(2026, 10, 16, 18, 0)));
        assert_eq!(p.title, "Пятничный стрим");
    }

    #[test]
    fn far_future_schedule_falls_back_to_habits() {
        let now = wednesday_noon();
        let schedule = vec![ScheduleSegment { start_ts: now + 20.0 * 86400.0, ..Default::default() }];
        let p = predict_next(&saturdays(), now, Some(&schedule), false, &Utc).unwrap();
        assert_eq!(p.source, Source::Habit);
    }

    #[test]
    fn realistic_patterns_match_the_python_version() {
        // те же случаи, что я гонял на Python: ежедневный стример, 5 недель из 8, ночные эфиры
        let now = wednesday_noon();
        let daily: Vec<f64> = (0..50).map(|i| ts(2026, 10, 13, 20, 5) - 86400.0 * i as f64 + (i % 4) as f64 * 600.0).collect();
        let p = predict(&daily, now).unwrap();
        assert_eq!((p.start_ts, p.level), (ts(2026, 10, 14, 20, 15), Level::High));

        let five_of_eight: Vec<f64> = [1, 2, 4, 5, 7]
            .iter()
            .map(|&i| ts(2026, 10, 17, 0, 0) - 7.0 * 86400.0 * i as f64 + (18 + i % 3) as f64 * 3600.0)
            .collect();
        let p = predict(&five_of_eight, now).unwrap();
        assert_eq!((p.weeks_with, p.weeks_total), (5, 8));
        assert_eq!(p.level, Level::Medium); // порог «высокой» 0,7, а тут 0,625 при ровном времени
    }
}
