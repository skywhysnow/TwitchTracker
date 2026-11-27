//! Сравнение двух проверок Twitch: кто вышел в эфир, сменил категорию или название, закончил эфир.

use std::collections::{BTreeMap, BTreeSet};

use crate::models::EventKind;
use crate::stream::LiveStream;

/// Сколько проверок подряд канал может «пропасть», прежде чем считаем его офлайн.
pub const MISS_GRACE: u32 = 2;

pub type StreamMap = BTreeMap<String, LiveStream>;

#[derive(Debug, Clone, PartialEq)]
pub struct StreamEvent {
    pub kind: EventKind,
    pub login: String,
    /// Новые данные (у «конца эфира» их нет).
    pub new: Option<LiveStream>,
    /// Прежние данные (у «начала эфира» их нет).
    pub old: Option<LiveStream>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiffResult {
    pub events: Vec<StreamEvent>,
    pub prev: StreamMap,
    pub misses: BTreeMap<String, u32>,
}

/// `prev == None` означает первую успешную проверку: запоминаем состояние и молчим.
/// `fresh`: только что добавленные каналы; если они уже в эфире, это не событие.
/// Короткий сбой API, при котором канал на одну проверку «пропал», не создаёт ложного «вышел в эфир».
pub fn diff_streams(
    prev: Option<&StreamMap>,
    current: &StreamMap,
    fresh: &BTreeSet<String>,
    misses: &BTreeMap<String, u32>,
    grace: u32,
) -> DiffResult {
    let Some(prev) = prev else {
        return DiffResult { events: vec![], prev: current.clone(), misses: BTreeMap::new() };
    };

    let mut base = prev.clone();
    for login in fresh {
        if let Some(cur) = current.get(login) {
            base.insert(login.clone(), cur.clone());
        }
    }

    let mut out = DiffResult::default();
    for (login, cur) in current {
        match base.get(login) {
            None => out.events.push(StreamEvent {
                kind: EventKind::Live,
                login: login.clone(),
                new: Some(cur.clone()),
                old: None,
            }),
            Some(old) => {
                if cur.game_key() != old.game_key() {
                    out.events.push(StreamEvent {
                        kind: EventKind::Game,
                        login: login.clone(),
                        new: Some(cur.clone()),
                        old: Some(old.clone()),
                    });
                }
                if cur.title != old.title {
                    out.events.push(StreamEvent {
                        kind: EventKind::Title,
                        login: login.clone(),
                        new: Some(cur.clone()),
                        old: Some(old.clone()),
                    });
                }
            }
        }
        out.prev.insert(login.clone(), cur.clone());
    }

    for (login, old) in &base {
        if current.contains_key(login) {
            continue;
        }
        let count = misses.get(login).copied().unwrap_or(0) + 1;
        if count < grace {
            out.prev.insert(login.clone(), old.clone());
            out.misses.insert(login.clone(), count);
        } else {
            out.events.push(StreamEvent {
                kind: EventKind::End,
                login: login.clone(),
                new: None,
                old: Some(old.clone()),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(login: &str, title: &str, game: &str) -> LiveStream {
        LiveStream {
            user_login: login.into(),
            title: title.into(),
            game_id: game.into(),
            game_name: game.into(),
            ..Default::default()
        }
    }

    fn map(items: &[LiveStream]) -> StreamMap {
        items.iter().map(|x| (x.user_login.clone(), x.clone())).collect()
    }

    fn kinds(result: &DiffResult) -> Vec<(EventKind, &str)> {
        result.events.iter().map(|e| (e.kind, e.login.as_str())).collect()
    }

    fn none() -> BTreeSet<String> {
        BTreeSet::new()
    }

    #[test]
    fn first_pass_is_silent() {
        let r = diff_streams(None, &map(&[s("a", "t", "g")]), &none(), &BTreeMap::new(), MISS_GRACE);
        assert!(r.events.is_empty());
        assert!(r.prev.contains_key("a"));
    }

    #[test]
    fn stream_start() {
        let r = diff_streams(Some(&StreamMap::new()), &map(&[s("a", "t", "g")]), &none(), &BTreeMap::new(), MISS_GRACE);
        assert_eq!(kinds(&r), vec![(EventKind::Live, "a")]);
    }

    #[test]
    fn category_and_title_changes() {
        let prev = map(&[s("a", "old", "g1")]);
        let r = diff_streams(Some(&prev), &map(&[s("a", "new", "g2")]), &none(), &BTreeMap::new(), MISS_GRACE);
        assert_eq!(kinds(&r), vec![(EventKind::Game, "a"), (EventKind::Title, "a")]);
    }

    #[test]
    fn no_change_no_events() {
        let prev = map(&[s("a", "t", "g")]);
        let r = diff_streams(Some(&prev), &prev, &none(), &BTreeMap::new(), MISS_GRACE);
        assert!(r.events.is_empty());
    }

    #[test]
    fn fresh_channel_already_live_is_silent() {
        let fresh: BTreeSet<String> = ["a".to_string()].into();
        let r = diff_streams(Some(&StreamMap::new()), &map(&[s("a", "t", "g")]), &fresh, &BTreeMap::new(), MISS_GRACE);
        assert!(r.events.is_empty());
        assert!(r.prev.contains_key("a"));
    }

    #[test]
    fn one_missed_poll_does_not_cause_false_live() {
        let prev = map(&[s("a", "t", "g")]);
        let first = diff_streams(Some(&prev), &StreamMap::new(), &none(), &BTreeMap::new(), MISS_GRACE);
        assert!(first.events.is_empty());
        assert!(first.prev.contains_key("a"));
        let second = diff_streams(Some(&first.prev), &map(&[s("a", "t", "g")]), &none(), &first.misses, MISS_GRACE);
        assert!(second.events.is_empty());
        assert!(second.misses.is_empty());
    }

    #[test]
    fn two_missed_polls_mean_end_then_new_live() {
        let prev = map(&[s("a", "t", "g")]);
        let first = diff_streams(Some(&prev), &StreamMap::new(), &none(), &BTreeMap::new(), MISS_GRACE);
        assert!(first.events.is_empty());
        let second = diff_streams(Some(&first.prev), &StreamMap::new(), &none(), &first.misses, MISS_GRACE);
        assert_eq!(kinds(&second), vec![(EventKind::End, "a")]);
        assert!(second.events[0].new.is_none() && second.events[0].old.is_some());
        assert!(!second.prev.contains_key("a"));
        let third = diff_streams(Some(&second.prev), &map(&[s("a", "t", "g")]), &none(), &second.misses, MISS_GRACE);
        assert_eq!(kinds(&third), vec![(EventKind::Live, "a")]);
    }

    #[test]
    fn category_key_falls_back_to_name() {
        let mk = |g: &str| LiveStream { user_login: "a".into(), title: "t".into(), game_name: g.into(), ..Default::default() };
        let prev = map(&[mk("X")]);
        let r = diff_streams(Some(&prev), &map(&[mk("Y")]), &none(), &BTreeMap::new(), MISS_GRACE);
        assert_eq!(kinds(&r), vec![(EventKind::Game, "a")]);
    }
}
