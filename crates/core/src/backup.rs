//! Резервная копия: каналы, группы, избранное и настройки уведомлений.
//! Токены входа не включаются. Формат файла совпадает с Python-версией.

use serde_json::{json, Map, Value};

use crate::models::{normalize_login, EventKind, DEFAULT_GROUP};
use crate::policy::parse_hhmm;
use crate::repository::Repository;

pub const APP_ID: &str = "TwitchTracker";
pub const BACKUP_VERSION: i64 = 1;

#[derive(Debug)]
pub enum BackupError {
    /// Это не файл резервной копии Twitch Tracker.
    NotBackup,
    /// Файл создан более новой версией программы.
    TooNew,
    Db(rusqlite::Error),
}

impl From<rusqlite::Error> for BackupError {
    fn from(e: rusqlite::Error) -> Self {
        BackupError::Db(e)
    }
}

impl BackupError {
    /// Код для интерфейса: по нему он подбирает текст на нужном языке.
    pub fn code(&self) -> &'static str {
        match self {
            BackupError::NotBackup => "not_backup",
            BackupError::TooNew => "too_new",
            BackupError::Db(_) => "db",
        }
    }
}

fn is_bool(v: &str) -> bool {
    v == "0" || v == "1"
}

fn int_in(v: &str, lo: u32, hi: u32) -> bool {
    !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()) && v.parse::<u32>().map_or(false, |n| (lo..=hi).contains(&n))
}

fn valid_setting(key: &str, value: &str) -> bool {
    match key {
        "language" => matches!(value, "auto" | "ru" | "en"),
        "poll_interval" => int_in(value, 10, 600),
        "quiet_start" | "quiet_end" => parse_hhmm(value).is_some(),
        "view_mode" => matches!(value, "cards" | "compact"),
        "sort_mode" => matches!(value, "default" | "viewers" | "duration"),
        "notifications" | "notify_live" | "notify_end" | "notify_game" | "notify_title" | "favorites_only"
        | "quiet_enabled" | "quiet_allow_favorites" | "quiet_digest" | "close_to_tray" => is_bool(value),
        _ => false,
    }
}

/// Только эти настройки попадают в копию и принимаются из неё. Всего остального (токены,
/// служебные флаги) здесь нет намеренно: копию можно спокойно пересылать и хранить.
const ALL_KEYS: [&str; 16] = [
    "language",
    "poll_interval",
    "notifications",
    "notify_live",
    "notify_end",
    "notify_game",
    "notify_title",
    "favorites_only",
    "quiet_enabled",
    "quiet_start",
    "quiet_end",
    "quiet_allow_favorites",
    "quiet_digest",
    "close_to_tray",
    "view_mode",
    "sort_mode",
];

pub fn export_data(repo: &Repository) -> Result<Value, BackupError> {
    let mut settings = Map::new();
    for key in ALL_KEYS {
        if let Some(value) = repo.get_setting(key)? {
            settings.insert(key.to_string(), Value::String(value));
        }
    }
    let streamers: Vec<Value> = repo
        .list_streamers()?
        .into_iter()
        .map(|s| {
            json!({
                "login": s.login,
                "display_name": s.display_name,
                "group": s.group,
                "favorite": s.favorite,
                "events": s.events,
            })
        })
        .collect();
    Ok(json!({
        "app": APP_ID,
        "version": BACKUP_VERSION,
        "exported_at": chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
        "groups": repo.list_groups()?,
        "streamers": streamers,
        "settings": settings,
    }))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportResult {
    pub added: usize,
    pub updated: usize,
    pub settings_applied: usize,
    pub skipped: usize,
}

fn setting_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Добавляет каналы из копии и обновляет совпавшие. Каналы, которых нет в файле, не трогает.
pub fn import_data(repo: &Repository, data: &Value) -> Result<ImportResult, BackupError> {
    if data.get("app").and_then(Value::as_str) != Some(APP_ID) {
        return Err(BackupError::NotBackup);
    }
    match data.get("version").and_then(Value::as_i64) {
        Some(v) if v <= BACKUP_VERSION => {}
        _ => return Err(BackupError::TooNew),
    }

    let mut result = ImportResult::default();
    if let Some(groups) = data.get("groups").and_then(Value::as_array) {
        for name in groups.iter().filter_map(Value::as_str) {
            if !name.trim().is_empty() && name.chars().count() <= 100 {
                repo.add_group(name.trim())?;
            }
        }
    }

    let mut existing: std::collections::HashSet<String> =
        repo.list_streamers()?.into_iter().map(|s| s.login).collect();
    if let Some(items) = data.get("streamers").and_then(Value::as_array) {
        for item in items {
            let login = item.get("login").and_then(Value::as_str).map(normalize_login).unwrap_or_default();
            if !item.is_object() || login.is_empty() {
                result.skipped += 1;
                continue;
            }
            let group: String = item
                .get("group")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|g| !g.is_empty())
                .unwrap_or(DEFAULT_GROUP)
                .chars()
                .take(100)
                .collect();
            repo.add_group(&group)?;
            if existing.contains(&login) {
                repo.set_group(&login, &group)?;
                result.updated += 1;
            } else {
                let name: String =
                    item.get("display_name").and_then(Value::as_str).unwrap_or("").chars().take(100).collect();
                repo.add_streamer(&login, &name, &group, "")?;
                existing.insert(login.clone());
                result.added += 1;
            }
            repo.set_favorite(&login, item.get("favorite").and_then(Value::as_bool).unwrap_or(false))?;
            if let Some(overrides) = item.get("events").and_then(Value::as_object) {
                for kind in EventKind::ALL {
                    match overrides.get(kind.as_str()) {
                        None | Some(Value::Null) => repo.set_event_override(&login, kind, None)?,
                        Some(Value::Bool(b)) => repo.set_event_override(&login, kind, Some(*b))?,
                        Some(_) => {}
                    }
                }
            }
        }
    }

    if let Some(settings) = data.get("settings").and_then(Value::as_object) {
        for key in ALL_KEYS {
            if let Some(raw) = settings.get(key) {
                let value = setting_text(raw);
                if valid_setting(key, &value) {
                    repo.set_setting(key, &value)?;
                    result.settings_applied += 1;
                } else {
                    result.skipped += 1;
                }
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::testing::TempRepo;

    #[test]
    fn roundtrip() {
        let (a, b) = (TempRepo::new(), TempRepo::new());
        a.repo.add_group("Друзья").unwrap();
        a.repo.add_streamer("amy", "Amy", "Друзья", "").unwrap();
        a.repo.set_favorite("amy", true).unwrap();
        a.repo.set_event_override("amy", EventKind::Title, Some(true)).unwrap();
        a.repo.set_event_override("amy", EventKind::End, Some(true)).unwrap();
        a.repo.set_setting("quiet_start", "22:30").unwrap();
        a.repo.set_bool("favorites_only", true).unwrap();
        a.repo.set_setting("poll_interval", "10").unwrap();

        let text = serde_json::to_string(&export_data(&a.repo).unwrap()).unwrap(); // как будто записали в файл
        let result = import_data(&b.repo, &serde_json::from_str(&text).unwrap()).unwrap();
        assert_eq!((result.added, result.updated), (1, 0));
        let s = b.repo.get_streamer("amy").unwrap().unwrap();
        assert_eq!((s.group.as_str(), s.favorite), ("Друзья", true));
        assert_eq!((s.events.title, s.events.end, s.events.live), (Some(true), Some(true), None));
        assert!(b.repo.list_groups().unwrap().contains(&"Друзья".to_string()));
        assert_eq!(b.repo.get_setting("quiet_start").unwrap().as_deref(), Some("22:30"));
        assert!(b.repo.get_bool("favorites_only", false));
        assert_eq!(b.repo.get_int("poll_interval", 60), 10);
    }

    #[test]
    fn tokens_never_exported() {
        let t = TempRepo::new();
        t.repo.set_setting("session_fallback", r#"{"refresh_token": "secret"}"#).unwrap();
        t.repo.set_setting("legacy_imported", "1").unwrap();
        let dumped = serde_json::to_string(&export_data(&t.repo).unwrap()).unwrap();
        assert!(!dumped.contains("secret") && !dumped.contains("session_fallback") && !dumped.contains("legacy_imported"));
    }

    #[test]
    fn import_updates_existing_and_keeps_others() {
        let t = TempRepo::new();
        t.repo.add_streamer("keep", "", DEFAULT_GROUP, "").unwrap();
        t.repo.add_streamer("amy", "Amy", DEFAULT_GROUP, "").unwrap();
        let data = json!({"app": "TwitchTracker", "version": 1,
            "streamers": [{"login": "amy", "group": "Новая", "favorite": true}]});
        let r = import_data(&t.repo, &data).unwrap();
        assert_eq!((r.added, r.updated), (0, 1));
        assert_eq!(t.repo.get_streamer("amy").unwrap().unwrap().group, "Новая");
        assert!(t.repo.get_streamer("keep").unwrap().is_some());
    }

    #[test]
    fn rejects_foreign_or_newer_files() {
        let t = TempRepo::new();
        assert!(matches!(import_data(&t.repo, &json!({"hello": "world"})), Err(BackupError::NotBackup)));
        assert!(matches!(import_data(&t.repo, &json!([1, 2, 3])), Err(BackupError::NotBackup)));
        assert!(matches!(
            import_data(&t.repo, &json!({"app": "TwitchTracker", "version": 99})),
            Err(BackupError::TooNew)
        ));
    }

    #[test]
    fn bad_values_are_skipped_not_applied() {
        let t = TempRepo::new();
        let data = json!({"app": "TwitchTracker", "version": 1,
            "streamers": [{"login": "bad login!"}, "oops", {"login": "ok_one"}],
            "settings": {"poll_interval": "5", "language": "klingon", "notify_live": "1", "evil": "x"}});
        let r = import_data(&t.repo, &data).unwrap();
        assert_eq!(r.added, 1);
        assert_eq!(r.skipped, 4); // два плохих канала и две плохие настройки
        assert_eq!(r.settings_applied, 1);
        assert_eq!(t.repo.get_setting("poll_interval").unwrap(), None);
        assert_eq!(t.repo.get_setting("evil").unwrap(), None);
    }

    #[test]
    fn interval_below_ten_is_rejected_and_numbers_are_accepted() {
        let t = TempRepo::new();
        let data = json!({"app": "TwitchTracker", "version": 1, "settings": {"poll_interval": "9"}});
        assert_eq!(import_data(&t.repo, &data).unwrap().settings_applied, 0);
        let data = json!({"app": "TwitchTracker", "version": 1, "settings": {"poll_interval": 30}});
        assert_eq!(import_data(&t.repo, &data).unwrap().settings_applied, 1);
    }
}
