//! Решает, показывать ли уведомление: типы событий, избранные, тихие часы.

use chrono::{NaiveTime, Timelike};

use crate::models::{EventKind, Overrides};
use crate::repository::Repository;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Ok,
    Disabled,
    EventOff,
    FavoritesOnly,
    Quiet,
}

/// Общие значения «уведомлять о» для каждого типа события.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventFlags {
    pub live: bool,
    pub end: bool,
    pub game: bool,
    pub title: bool,
}

impl Default for EventFlags {
    fn default() -> Self {
        EventFlags { live: true, end: false, game: false, title: false }
    }
}

impl EventFlags {
    pub fn get(&self, kind: EventKind) -> bool {
        match kind {
            EventKind::Live => self.live,
            EventKind::End => self.end,
            EventKind::Game => self.game,
            EventKind::Title => self.title,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NotifySettings {
    pub enabled: bool,
    pub events: EventFlags,
    pub favorites_only: bool,
    pub quiet_enabled: bool,
    pub quiet_start: NaiveTime,
    pub quiet_end: NaiveTime,
    pub quiet_allow_favorites: bool,
    /// После тихих часов показать одну сводку.
    pub quiet_digest: bool,
}

fn hm(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).expect("корректное время")
}

impl Default for NotifySettings {
    fn default() -> Self {
        NotifySettings {
            enabled: true,
            events: EventFlags::default(),
            favorites_only: false,
            quiet_enabled: false,
            quiet_start: hm(23, 0),
            quiet_end: hm(9, 0),
            quiet_allow_favorites: false,
            quiet_digest: true,
        }
    }
}

impl NotifySettings {
    pub fn load(repo: &Repository) -> NotifySettings {
        let d = NotifySettings::default();
        let time_setting = |key: &str, default: NaiveTime| {
            repo.get_setting(key).ok().flatten().and_then(|v| parse_hhmm(&v)).unwrap_or(default)
        };
        NotifySettings {
            enabled: repo.get_bool("notifications", d.enabled),
            events: EventFlags {
                live: repo.get_bool("notify_live", d.events.live),
                end: repo.get_bool("notify_end", d.events.end),
                game: repo.get_bool("notify_game", d.events.game),
                title: repo.get_bool("notify_title", d.events.title),
            },
            favorites_only: repo.get_bool("favorites_only", d.favorites_only),
            quiet_enabled: repo.get_bool("quiet_enabled", d.quiet_enabled),
            quiet_start: time_setting("quiet_start", d.quiet_start),
            quiet_end: time_setting("quiet_end", d.quiet_end),
            quiet_allow_favorites: repo.get_bool("quiet_allow_favorites", d.quiet_allow_favorites),
            quiet_digest: repo.get_bool("quiet_digest", d.quiet_digest),
        }
    }

    pub fn quiet_now(&self, now: NaiveTime) -> bool {
        self.quiet_enabled && is_quiet(now, self.quiet_start, self.quiet_end)
    }
}

/// `9:05` и `09:05` подходят, `24:00`, `12:60` и `abc` нет.
pub fn parse_hhmm(text: &str) -> Option<NaiveTime> {
    let (h, m) = text.trim().split_once(':')?;
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    if !digits(h) || !digits(m) || h.len() > 2 || m.len() != 2 {
        return None;
    }
    NaiveTime::from_hms_opt(h.parse().ok()?, m.parse().ok()?, 0)
}

pub fn format_hhmm(t: NaiveTime) -> String {
    format!("{:02}:{:02}", t.hour(), t.minute())
}

/// Попадает ли время в тихие часы. Интервал может переходить через полночь (23:00-09:00).
pub fn is_quiet(now: NaiveTime, start: NaiveTime, end: NaiveTime) -> bool {
    if start == end {
        return false;
    }
    if start < end {
        start <= now && now < end
    } else {
        now >= start || now < end
    }
}

/// Своё значение канала важнее общего; `None` значит «как в общих настройках».
pub fn event_enabled(kind: EventKind, overrides: &Overrides, settings: &NotifySettings) -> bool {
    overrides.get(kind).unwrap_or_else(|| settings.events.get(kind))
}

pub fn decide(
    kind: EventKind,
    favorite: bool,
    overrides: &Overrides,
    settings: &NotifySettings,
    now: NaiveTime,
) -> (bool, Reason) {
    if !settings.enabled {
        return (false, Reason::Disabled);
    }
    if !event_enabled(kind, overrides, settings) {
        return (false, Reason::EventOff);
    }
    if settings.favorites_only && !favorite {
        return (false, Reason::FavoritesOnly);
    }
    if settings.quiet_now(now) && !(settings.quiet_allow_favorites && favorite) {
        return (false, Reason::Quiet);
    }
    (true, Reason::Ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::testing::TempRepo;

    const NOON: fn() -> NaiveTime = || hm(12, 0);
    const NIGHT: fn() -> NaiveTime = || hm(2, 0);

    fn quiet_settings() -> NotifySettings {
        NotifySettings { quiet_enabled: true, ..Default::default() }
    }

    #[test]
    fn overnight_window() {
        let (start, end) = (hm(23, 0), hm(9, 0));
        assert!(is_quiet(hm(23, 30), start, end));
        assert!(is_quiet(hm(3, 0), start, end));
        assert!(is_quiet(hm(23, 0), start, end));
        assert!(!is_quiet(hm(9, 0), start, end));
        assert!(!is_quiet(hm(15, 0), start, end));
    }

    #[test]
    fn same_day_window_and_equal_bounds() {
        assert!(is_quiet(hm(13, 0), hm(12, 0), hm(14, 0)));
        assert!(!is_quiet(hm(14, 0), hm(12, 0), hm(14, 0)));
        assert!(!is_quiet(hm(5, 0), hm(5, 0), hm(5, 0)));
    }

    #[test]
    fn parse_and_format() {
        assert_eq!(parse_hhmm("9:05"), Some(hm(9, 5)));
        assert_eq!(parse_hhmm("23:00"), Some(hm(23, 0)));
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("12:60"), None);
        assert_eq!(parse_hhmm("abc"), None);
        assert_eq!(parse_hhmm("12:5"), None);
        assert_eq!(format_hhmm(hm(9, 5)), "09:05");
    }

    #[test]
    fn defaults_notify_only_stream_start() {
        let s = NotifySettings::default();
        let none = Overrides::default();
        assert_eq!(decide(EventKind::Live, false, &none, &s, NOON()), (true, Reason::Ok));
        for kind in [EventKind::End, EventKind::Game, EventKind::Title] {
            assert_eq!(decide(kind, false, &none, &s, NOON()), (false, Reason::EventOff));
        }
    }

    #[test]
    fn channel_override_beats_global() {
        let s = NotifySettings::default();
        let o = Overrides { title: Some(true), live: Some(false), ..Default::default() };
        assert!(event_enabled(EventKind::Title, &o, &s));
        assert!(!event_enabled(EventKind::Live, &o, &s));
        let end_on = Overrides { end: Some(true), ..Default::default() };
        assert_eq!(decide(EventKind::End, false, &end_on, &s, NOON()), (true, Reason::Ok));
    }

    #[test]
    fn master_switch_favorites_and_quiet() {
        let none = Overrides::default();
        let off = NotifySettings { enabled: false, ..Default::default() };
        assert_eq!(decide(EventKind::Live, true, &none, &off, NOON()), (false, Reason::Disabled));

        let fav = NotifySettings { favorites_only: true, ..Default::default() };
        assert_eq!(decide(EventKind::Live, false, &none, &fav, NOON()), (false, Reason::FavoritesOnly));
        assert_eq!(decide(EventKind::Live, true, &none, &fav, NOON()), (true, Reason::Ok));

        let quiet = quiet_settings();
        assert_eq!(decide(EventKind::Live, true, &none, &quiet, NIGHT()), (false, Reason::Quiet));
        assert_eq!(decide(EventKind::Live, true, &none, &quiet, NOON()), (true, Reason::Ok));

        let let_favs = NotifySettings { quiet_allow_favorites: true, ..quiet_settings() };
        assert_eq!(decide(EventKind::Live, true, &none, &let_favs, NIGHT()), (true, Reason::Ok));
        assert_eq!(decide(EventKind::Live, false, &none, &let_favs, NIGHT()), (false, Reason::Quiet));
    }

    #[test]
    fn load_from_repo_with_broken_values() {
        let t = TempRepo::new();
        assert_eq!(NotifySettings::load(&t.repo), NotifySettings::default());
        t.repo.set_bool("notify_title", true).unwrap();
        t.repo.set_bool("quiet_enabled", true).unwrap();
        t.repo.set_setting("quiet_start", "22:30").unwrap();
        t.repo.set_setting("quiet_end", "garbage").unwrap();
        let n = NotifySettings::load(&t.repo);
        assert!(n.events.title);
        assert_eq!((n.quiet_start, n.quiet_end), (hm(22, 30), hm(9, 0)));
        assert!(n.quiet_now(hm(23, 0)));
    }
}
