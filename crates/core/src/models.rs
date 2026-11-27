use serde::{Deserialize, Serialize};

pub const DEFAULT_GROUP: &str = "Общее";

/// Тип события на канале. Строковые имена совпадают с Python-версией и с таблицей `events`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventKind {
    Live,
    End,
    Game,
    Title,
}

impl EventKind {
    pub const ALL: [EventKind; 4] = [EventKind::Live, EventKind::End, EventKind::Game, EventKind::Title];

    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Live => "live",
            EventKind::End => "end",
            EventKind::Game => "game",
            EventKind::Title => "title",
        }
    }

    pub fn parse(text: &str) -> Option<EventKind> {
        EventKind::ALL.into_iter().find(|k| k.as_str() == text)
    }
}

/// Своё значение канала для каждого типа события: `None` = «как в общих настройках».
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overrides {
    pub live: Option<bool>,
    pub end: Option<bool>,
    pub game: Option<bool>,
    pub title: Option<bool>,
}

impl Overrides {
    pub fn get(&self, kind: EventKind) -> Option<bool> {
        match kind {
            EventKind::Live => self.live,
            EventKind::End => self.end,
            EventKind::Game => self.game,
            EventKind::Title => self.title,
        }
    }

    pub fn set(&mut self, kind: EventKind, value: Option<bool>) {
        match kind {
            EventKind::Live => self.live = value,
            EventKind::End => self.end = value,
            EventKind::Game => self.game = value,
            EventKind::Title => self.title = value,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Streamer {
    pub login: String,
    pub display_name: String,
    pub group: String,
    pub favorite: bool,
    pub avatar_url: String,
    /// Когда последний раз видели в эфире (unix time).
    pub last_live: Option<f64>,
    pub twitch_id: String,
    /// Когда подтягивали архив прошлых эфиров.
    pub vod_synced: Option<f64>,
    pub events: Overrides,
}

impl Streamer {
    pub fn new(login: &str) -> Self {
        Streamer {
            login: login.to_string(),
            display_name: String::new(),
            group: DEFAULT_GROUP.to_string(),
            favorite: false,
            avatar_url: String::new(),
            last_live: None,
            twitch_id: String::new(),
            vod_synced: None,
            events: Overrides::default(),
        }
    }

    pub fn name(&self) -> &str {
        if self.display_name.is_empty() {
            &self.login
        } else {
            &self.display_name
        }
    }
}

/// `https://twitch.tv/Shroud?x=1`, `@shroud`, `Shroud` -> `shroud`. Пустая строка, если это не логин.
pub fn normalize_login(text: &str) -> String {
    let lowered = text.trim().to_lowercase();
    let mut t = lowered.trim_start_matches('@');
    if let Some(pos) = t.find("twitch.tv/") {
        t = &t[pos + "twitch.tv/".len()..];
    }
    let t = t.split('/').next().unwrap_or("");
    let t = t.split('?').next().unwrap_or("");
    let valid = !t.is_empty()
        && t.chars().count() <= 25
        && t.chars().all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_'));
    if valid {
        t.to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_variants() {
        assert_eq!(normalize_login("Shroud"), "shroud");
        assert_eq!(normalize_login("@shroud"), "shroud");
        assert_eq!(normalize_login("https://www.twitch.tv/Shroud?sr=a"), "shroud");
        assert_eq!(normalize_login("twitch.tv/shroud/videos"), "shroud");
        assert_eq!(normalize_login("плохой логин"), "");
        assert_eq!(normalize_login(""), "");
        assert_eq!(normalize_login(&"a".repeat(26)), "");
    }

    #[test]
    fn event_kind_roundtrip() {
        for kind in EventKind::ALL {
            assert_eq!(EventKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(EventKind::parse("nope"), None);
    }

    #[test]
    fn overrides_get_set() {
        let mut o = Overrides::default();
        o.set(EventKind::Title, Some(true));
        assert_eq!(o.get(EventKind::Title), Some(true));
        assert_eq!(o.get(EventKind::Live), None);
    }
}
