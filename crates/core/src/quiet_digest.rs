//! Сводка «что было, пока шли тихие часы».

use std::collections::{BTreeMap, HashSet};

/// Начало эфира, замеченное за тихие часы.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveEvent {
    pub login: String,
    pub name: String,
    pub ts: f64,
}

/// (имена тех, кто всё ещё в эфире, сколько уже закончили) или `None`, если никто не выходил в эфир.
pub fn summarize_quiet(events: &[LiveEvent], live_logins: &HashSet<String>) -> Option<(Vec<String>, usize)> {
    let mut ordered: Vec<&LiveEvent> = events.iter().collect();
    ordered.sort_by(|a, b| a.ts.partial_cmp(&b.ts).unwrap());

    let mut started: BTreeMap<&str, (&str, f64)> = BTreeMap::new();
    for e in ordered {
        started.entry(&e.login).or_insert((&e.name, e.ts));
    }
    if started.is_empty() {
        return None;
    }
    let mut still: Vec<(&str, f64)> = started
        .iter()
        .filter(|(login, _)| live_logins.contains(**login))
        .map(|(_, (name, ts))| (*name, *ts))
        .collect();
    still.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap()); // в порядке выхода в эфир
    let ended = started.len() - still.len();
    Some((still.into_iter().map(|(n, _)| n.to_string()).collect(), ended))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(login: &str, ts: f64) -> LiveEvent {
        let mut name = login.to_string();
        name[..1].make_ascii_uppercase();
        LiveEvent { login: login.into(), name, ts }
    }

    #[test]
    fn nothing_started() {
        assert!(summarize_quiet(&[], &HashSet::new()).is_none());
    }

    #[test]
    fn still_live_and_ended() {
        let events = [ev("a", 3.0), ev("b", 1.0), ev("c", 2.0)];
        let live: HashSet<String> = ["a".to_string(), "c".to_string()].into();
        let (still, ended) = summarize_quiet(&events, &live).unwrap();
        assert_eq!(still, vec!["C".to_string(), "A".to_string()]);
        assert_eq!(ended, 1);
    }

    #[test]
    fn same_channel_twice_counts_once() {
        let events = [ev("a", 1.0), ev("a", 5.0)];
        assert_eq!(summarize_quiet(&events, &HashSet::new()), Some((vec![], 1)));
    }
}
