//! Статистика эфиров за период. Чистые функции, без базы.

use std::collections::BTreeMap;

use crate::repository::StreamRow;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    pub stream_count: usize,
    pub channel_count: usize,
    pub total_seconds: f64,
    /// (логин, секунды, число эфиров), от большего к меньшему.
    pub top_channels: Vec<(String, f64, usize)>,
    /// (категория, секунды), от большего к меньшему.
    pub top_games: Vec<(String, f64)>,
}

impl Stats {
    pub fn average_seconds(&self) -> f64 {
        if self.stream_count == 0 {
            0.0
        } else {
            self.total_seconds / self.stream_count as f64
        }
    }
}

/// Считаем только ту часть эфира, что попала в окно `[since, now]`.
pub fn compute_stats(rows: &[StreamRow], since: f64, now: f64, top: usize) -> Stats {
    let mut channels: BTreeMap<String, (f64, usize)> = BTreeMap::new();
    let mut games: BTreeMap<String, f64> = BTreeMap::new();
    let mut stats = Stats::default();

    for r in rows {
        let end = r.ended_ts.unwrap_or(now);
        let full = (end - r.started_ts).max(0.0);
        let clipped = (end.min(now) - r.started_ts.max(since)).max(0.0);
        if clipped <= 0.0 {
            continue;
        }
        stats.stream_count += 1;
        stats.total_seconds += clipped;
        let entry = channels.entry(r.login.clone()).or_insert((0.0, 0));
        entry.0 += clipped;
        entry.1 += 1;

        let mut seen: BTreeMap<&str, f64> =
            r.games.iter().filter(|(g, s)| !g.is_empty() && **s > 0.0).map(|(g, s)| (g.as_str(), *s)).collect();
        if seen.is_empty() && !r.game.is_empty() {
            seen.insert(&r.game, if full > 0.0 { full } else { clipped });
        }
        let scale = if full > 0.0 { clipped / full } else { 1.0 };
        for (game, secs) in seen {
            *games.entry(game.to_string()).or_insert(0.0) += secs * scale.min(1.0);
        }
    }

    stats.channel_count = channels.len();
    let mut top_channels: Vec<(String, f64, usize)> = channels.into_iter().map(|(l, (s, n))| (l, s, n)).collect();
    top_channels.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    top_channels.truncate(top);
    stats.top_channels = top_channels;
    let mut top_games: Vec<(String, f64)> = games.into_iter().collect();
    top_games.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    top_games.truncate(top);
    stats.top_games = top_games;
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 1_800_000_000.0;
    const H: f64 = 3600.0;

    fn row(login: &str, start: f64, end: Option<f64>, game: &str, games: &[(&str, f64)]) -> StreamRow {
        StreamRow {
            stream_id: format!("{login}{start}"),
            login: login.into(),
            started_ts: start,
            ended_ts: end,
            last_seen: end.unwrap_or(NOW),
            game: game.into(),
            title: String::new(),
            peak_viewers: 0,
            games: games.iter().map(|(g, s)| (g.to_string(), *s)).collect(),
        }
    }

    #[test]
    fn totals_and_tops() {
        let rows = vec![
            row("a", NOW - 10.0 * H, Some(NOW - 8.0 * H), "Chat", &[("Chat", 3600.0), ("GTA", 3600.0)]),
            row("a", NOW - 5.0 * H, Some(NOW - 4.5 * H), "GTA", &[]),
            row("b", NOW - 3.0 * H, None, "Chat", &[]),
        ];
        let s = compute_stats(&rows, NOW - 86400.0, NOW, 5);
        assert_eq!((s.stream_count, s.channel_count), (3, 2));
        assert!((s.total_seconds - (2.0 * H + 0.5 * H + 3.0 * H)).abs() < 1e-6);
        assert_eq!(s.top_channels[0].0, "b");
        assert_eq!(s.top_channels.iter().find(|c| c.0 == "a").unwrap().2, 2);
        let gta = s.top_games.iter().find(|g| g.0 == "GTA").unwrap().1;
        assert!((gta - (3600.0 + 1800.0)).abs() < 1e-6);
    }

    #[test]
    fn stream_is_clipped_to_window() {
        let rows = vec![row("a", NOW - 34.0 * H, Some(NOW - 23.0 * H), "Chat", &[])];
        let s = compute_stats(&rows, NOW - 24.0 * H, NOW, 5);
        assert!((s.total_seconds - H).abs() < 1e-6);
    }

    #[test]
    fn outside_window_ignored() {
        let rows = vec![row("a", NOW - 100.0 * H, Some(NOW - 99.0 * H), "Chat", &[])];
        let s = compute_stats(&rows, NOW - 86400.0, NOW, 5);
        assert_eq!((s.stream_count, s.total_seconds, s.average_seconds()), (0, 0.0, 0.0));
    }
}
