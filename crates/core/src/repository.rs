//! Хранилище на SQLite. Схема и имена колонок совпадают с Python-версией.
//!
//! Соединение открывается на каждую операцию, поэтому `Repository` можно клонировать
//! и пользоваться им из окна и из потока монитора одновременно.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};

use crate::models::{EventKind, Overrides, Streamer, DEFAULT_GROUP};
use crate::prediction::ScheduleSegment;

pub type Result<T> = rusqlite::Result<T>;

/// Столько последних событий хранится в ленте.
pub const EVENT_LIMIT: i64 = 1000;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS groups (name TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS streamers (
    login TEXT PRIMARY KEY,
    display_name TEXT NOT NULL DEFAULT '',
    group_name TEXT NOT NULL DEFAULT 'Общее',
    favorite INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts REAL NOT NULL,
    kind TEXT NOT NULL,
    login TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    game TEXT NOT NULL DEFAULT '',
    title TEXT NOT NULL DEFAULT '',
    old_game TEXT NOT NULL DEFAULT '',
    old_title TEXT NOT NULL DEFAULT '',
    duration REAL);
CREATE TABLE IF NOT EXISTS streams (
    stream_id TEXT PRIMARY KEY,
    login TEXT NOT NULL,
    started_ts REAL NOT NULL,
    ended_ts REAL,
    last_seen REAL NOT NULL,
    game TEXT NOT NULL DEFAULT '',
    title TEXT NOT NULL DEFAULT '',
    peak_viewers INTEGER NOT NULL DEFAULT 0,
    games TEXT NOT NULL DEFAULT '{}');
CREATE INDEX IF NOT EXISTS idx_streams_started ON streams(started_ts);
CREATE TABLE IF NOT EXISTS schedules (
    login TEXT PRIMARY KEY,
    fetched_ts REAL NOT NULL,
    segments TEXT NOT NULL DEFAULT '[]');
";

/// Колонки, добавленные после первой версии: старые базы дополняются на месте.
const ADDED_COLUMNS: [(&str, &str); 8] = [
    ("avatar_url", "TEXT NOT NULL DEFAULT ''"),
    ("last_live", "REAL"),
    ("ev_live", "INTEGER"),
    ("ev_game", "INTEGER"),
    ("ev_title", "INTEGER"),
    ("ev_end", "INTEGER"),
    ("twitch_id", "TEXT NOT NULL DEFAULT ''"),
    ("vod_synced", "REAL"),
];

const SELECT: &str = "login, display_name, group_name, favorite, avatar_url, last_live, \
                      ev_live, ev_game, ev_title, ev_end, twitch_id, vod_synced";

fn event_column(kind: EventKind) -> &'static str {
    match kind {
        EventKind::Live => "ev_live",
        EventKind::End => "ev_end",
        EventKind::Game => "ev_game",
        EventKind::Title => "ev_title",
    }
}

fn tri(value: Option<i64>) -> Option<bool> {
    value.map(|v| v != 0)
}

fn to_streamer(r: &rusqlite::Row) -> Result<Streamer> {
    Ok(Streamer {
        login: r.get(0)?,
        display_name: r.get(1)?,
        group: r.get(2)?,
        favorite: r.get::<_, i64>(3)? != 0,
        avatar_url: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
        last_live: r.get(5)?,
        events: Overrides {
            live: tri(r.get(6)?),
            game: tri(r.get(7)?),
            title: tri(r.get(8)?),
            end: tri(r.get(9)?),
        },
        twitch_id: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
        vod_synced: r.get(11)?,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewEvent {
    pub kind: EventKind,
    pub login: String,
    pub name: String,
    pub game: String,
    pub title: String,
    pub old_game: String,
    pub old_title: String,
    pub duration: Option<f64>,
    pub ts: f64,
}

impl NewEvent {
    pub fn new(kind: EventKind, login: &str, ts: f64) -> Self {
        NewEvent {
            kind,
            login: login.into(),
            name: String::new(),
            game: String::new(),
            title: String::new(),
            old_game: String::new(),
            old_title: String::new(),
            duration: None,
            ts,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventRow {
    pub id: i64,
    pub ts: f64,
    pub kind: String,
    pub login: String,
    pub name: String,
    pub game: String,
    pub title: String,
    pub old_game: String,
    pub old_title: String,
    pub duration: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamRow {
    pub stream_id: String,
    pub login: String,
    pub started_ts: f64,
    pub ended_ts: Option<f64>,
    pub last_seen: f64,
    pub game: String,
    pub title: String,
    pub peak_viewers: i64,
    /// Секунды, которые программа видела эфир в каждой категории.
    pub games: BTreeMap<String, f64>,
}

/// Обновление идущего эфира.
#[derive(Debug, Clone)]
pub struct StreamUpdate<'a> {
    pub stream_id: &'a str,
    pub login: &'a str,
    pub started_ts: f64,
    pub now: f64,
    pub game: &'a str,
    pub title: &'a str,
    pub viewers: i64,
    pub seen_seconds: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredSchedule {
    pub fetched_ts: f64,
    pub segments: Vec<ScheduleSegment>,
}

const STREAM_COLS: &str = "stream_id, login, started_ts, ended_ts, last_seen, game, title, peak_viewers, games";

fn to_stream_row(r: &rusqlite::Row) -> Result<StreamRow> {
    let games_json: String = r.get::<_, Option<String>>(8)?.unwrap_or_else(|| "{}".into());
    Ok(StreamRow {
        stream_id: r.get(0)?,
        login: r.get(1)?,
        started_ts: r.get(2)?,
        ended_ts: r.get(3)?,
        last_seen: r.get(4)?,
        game: r.get(5)?,
        title: r.get(6)?,
        peak_viewers: r.get(7)?,
        games: serde_json::from_str(&games_json).unwrap_or_default(),
    })
}

#[derive(Debug, Clone)]
pub struct Repository {
    path: PathBuf,
}

impl Repository {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let repo = Repository { path: path.as_ref().to_path_buf() };
        if let Some(dir) = repo.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        repo.init_db()?;
        Ok(repo)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn conn(&self) -> Result<Connection> {
        let conn = Connection::open(&self.path)?;
        conn.busy_timeout(Duration::from_secs(10))?;
        Ok(conn)
    }

    fn init_db(&self) -> Result<()> {
        let conn = self.conn()?;
        conn.execute_batch(SCHEMA)?;
        let existing: HashSet<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(streamers)")?;
            let names = stmt.query_map([], |r| r.get::<_, String>(1))?;
            names.collect::<Result<_>>()?
        };
        for (name, ddl) in ADDED_COLUMNS {
            if !existing.contains(name) {
                conn.execute(&format!("ALTER TABLE streamers ADD COLUMN {name} {ddl}"), [])?;
            }
        }
        conn.execute("INSERT OR IGNORE INTO groups(name) VALUES (?1)", [DEFAULT_GROUP])?;
        Ok(())
    }

    // ---------- настройки ----------
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.conn()?
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| r.get(0))
            .optional()
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn()?
            .execute("INSERT OR REPLACE INTO settings(key, value) VALUES (?1, ?2)", params![key, value])?;
        Ok(())
    }

    pub fn get_int(&self, key: &str, default: i64) -> i64 {
        self.get_setting(key).ok().flatten().and_then(|v| v.trim().parse().ok()).unwrap_or(default)
    }

    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        match self.get_setting(key) {
            Ok(Some(v)) => v == "1",
            _ => default,
        }
    }

    pub fn set_bool(&self, key: &str, value: bool) -> Result<()> {
        self.set_setting(key, if value { "1" } else { "0" })
    }

    // ---------- группы ----------
    pub fn list_groups(&self) -> Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT name FROM groups ORDER BY rowid")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect()
    }

    pub fn add_group(&self, name: &str) -> Result<bool> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(false);
        }
        Ok(self.conn()?.execute("INSERT OR IGNORE INTO groups(name) VALUES (?1)", [name])? > 0)
    }

    /// Удаляет группу; её стримеры переезжают в группу по умолчанию.
    pub fn delete_group(&self, name: &str) -> Result<()> {
        if name == DEFAULT_GROUP {
            return Ok(());
        }
        let conn = self.conn()?;
        conn.execute("UPDATE streamers SET group_name=?1 WHERE group_name=?2", params![DEFAULT_GROUP, name])?;
        conn.execute("DELETE FROM groups WHERE name=?1", [name])?;
        Ok(())
    }

    // ---------- стримеры ----------
    pub fn list_streamers(&self) -> Result<Vec<Streamer>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!("SELECT {SELECT} FROM streamers ORDER BY rowid"))?;
        let rows = stmt.query_map([], to_streamer)?;
        rows.collect()
    }

    pub fn get_streamer(&self, login: &str) -> Result<Option<Streamer>> {
        self.conn()?
            .query_row(&format!("SELECT {SELECT} FROM streamers WHERE login=?1"), [login], to_streamer)
            .optional()
    }

    /// `true`, если канал добавлен; `false`, если он уже есть.
    pub fn add_streamer(&self, login: &str, display_name: &str, group: &str, avatar_url: &str) -> Result<bool> {
        let conn = self.conn()?;
        conn.execute("INSERT OR IGNORE INTO groups(name) VALUES (?1)", [group])?;
        let added = conn.execute(
            "INSERT OR IGNORE INTO streamers(login, display_name, group_name, avatar_url) VALUES (?1, ?2, ?3, ?4)",
            params![login, display_name, group, avatar_url],
        )?;
        Ok(added > 0)
    }

    pub fn remove_streamer(&self, login: &str) -> Result<()> {
        self.conn()?.execute("DELETE FROM streamers WHERE login=?1", [login])?;
        Ok(())
    }

    pub fn set_group(&self, login: &str, group: &str) -> Result<()> {
        self.conn()?.execute("UPDATE streamers SET group_name=?1 WHERE login=?2", params![group, login])?;
        Ok(())
    }

    pub fn set_favorite(&self, login: &str, favorite: bool) -> Result<()> {
        self.conn()?.execute("UPDATE streamers SET favorite=?1 WHERE login=?2", params![favorite as i64, login])?;
        Ok(())
    }

    /// Своё значение для типа события: `Some(true/false)` либо `None` = как в общих настройках.
    pub fn set_event_override(&self, login: &str, kind: EventKind, value: Option<bool>) -> Result<()> {
        let sql = format!("UPDATE streamers SET {}=?1 WHERE login=?2", event_column(kind));
        self.conn()?.execute(&sql, params![value.map(|v| v as i64), login])?;
        Ok(())
    }

    /// Пустые значения ничего не затирают.
    pub fn set_profile(&self, login: &str, display_name: &str, avatar_url: &str, twitch_id: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE streamers SET display_name=CASE WHEN ?1<>'' THEN ?1 ELSE display_name END, \
             avatar_url=CASE WHEN ?2<>'' THEN ?2 ELSE avatar_url END, \
             twitch_id=CASE WHEN ?3<>'' THEN ?3 ELSE twitch_id END WHERE login=?4",
            params![display_name, avatar_url, twitch_id, login],
        )?;
        Ok(())
    }

    pub fn mark_live(&self, logins: &[String], timestamp: f64) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        for login in logins {
            tx.execute("UPDATE streamers SET last_live=?1 WHERE login=?2", params![timestamp, login])?;
        }
        tx.commit()
    }

    pub fn mark_vod_synced(&self, login: &str, timestamp: f64) -> Result<()> {
        self.conn()?.execute("UPDATE streamers SET vod_synced=?1 WHERE login=?2", params![timestamp, login])?;
        Ok(())
    }

    // ---------- лента событий ----------
    pub fn add_event(&self, e: &NewEvent) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO events(ts, kind, login, name, game, title, old_game, old_title, duration) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![e.ts, e.kind.as_str(), e.login, e.name, e.game, e.title, e.old_game, e.old_title, e.duration],
        )?;
        conn.execute("DELETE FROM events WHERE id <= (SELECT MAX(id) FROM events) - ?1", [EVENT_LIMIT])?;
        Ok(())
    }

    /// Новые сверху.
    pub fn list_events(&self, limit: i64) -> Result<Vec<EventRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, ts, kind, login, name, game, title, old_game, old_title, duration \
             FROM events ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |r| {
            Ok(EventRow {
                id: r.get(0)?,
                ts: r.get(1)?,
                kind: r.get(2)?,
                login: r.get(3)?,
                name: r.get(4)?,
                game: r.get(5)?,
                title: r.get(6)?,
                old_game: r.get(7)?,
                old_title: r.get(8)?,
                duration: r.get(9)?,
            })
        })?;
        rows.collect()
    }

    pub fn clear_events(&self) -> Result<()> {
        self.conn()?.execute("DELETE FROM events", [])?;
        Ok(())
    }

    // ---------- история эфиров ----------
    /// Запоминает, что эфир идёт; `seen_seconds` прибавляется к текущей категории.
    pub fn upsert_stream(&self, u: &StreamUpdate) -> Result<()> {
        let conn = self.conn()?;
        let existing: Option<(String, i64)> = conn
            .query_row("SELECT games, peak_viewers FROM streams WHERE stream_id=?1", [u.stream_id], |r| {
                Ok((r.get::<_, Option<String>>(0)?.unwrap_or_else(|| "{}".into()), r.get(1)?))
            })
            .optional()?;
        let mut games: BTreeMap<String, f64> = existing
            .as_ref()
            .and_then(|(json, _)| serde_json::from_str(json).ok())
            .unwrap_or_default();
        if !u.game.is_empty() && u.seen_seconds > 0.0 {
            *games.entry(u.game.to_string()).or_insert(0.0) += u.seen_seconds;
        }
        let games_json = serde_json::to_string(&games).unwrap_or_else(|_| "{}".into());
        match existing {
            None => conn.execute(
                "INSERT INTO streams(stream_id, login, started_ts, last_seen, game, title, peak_viewers, games) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![u.stream_id, u.login, u.started_ts, u.now, u.game, u.title, u.viewers, games_json],
            )?,
            Some((_, peak)) => conn.execute(
                "UPDATE streams SET login=?1, last_seen=?2, ended_ts=NULL, game=?3, title=?4, peak_viewers=?5, games=?6 \
                 WHERE stream_id=?7",
                params![u.login, u.now, u.game, u.title, u.viewers.max(peak), games_json, u.stream_id],
            )?,
        };
        Ok(())
    }

    /// Эфиры, которых больше нет среди идущих, закрываются временем последней проверки.
    pub fn close_open_streams(&self, keep: &HashSet<String>) -> Result<()> {
        let conn = self.conn()?;
        let open: Vec<String> = {
            let mut stmt = conn.prepare("SELECT stream_id FROM streams WHERE ended_ts IS NULL")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<Result<_>>()?
        };
        for id in open.iter().filter(|id| !keep.contains(*id)) {
            conn.execute("UPDATE streams SET ended_ts=last_seen WHERE stream_id=?1", [id])?;
        }
        Ok(())
    }

    pub fn get_stream(&self, stream_id: &str) -> Result<Option<StreamRow>> {
        self.conn()?
            .query_row(&format!("SELECT {STREAM_COLS} FROM streams WHERE stream_id=?1"), [stream_id], to_stream_row)
            .optional()
    }

    /// Эфиры, которые шли в период с `since_ts` (идущие сейчас тоже). Новые сверху.
    pub fn list_streams_since(&self, since_ts: f64) -> Result<Vec<StreamRow>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {STREAM_COLS} FROM streams WHERE ended_ts IS NULL OR ended_ts >= ?1 ORDER BY started_ts DESC"
        ))?;
        let rows = stmt.query_map([since_ts], to_stream_row)?;
        rows.collect()
    }

    /// Эфир из архива Twitch. Если программа уже записала этот эфир сама, её запись не трогаем.
    pub fn import_past_stream(
        &self,
        stream_id: &str,
        login: &str,
        started_ts: f64,
        ended_ts: f64,
        title: &str,
    ) -> Result<()> {
        self.conn()?.execute(
            "INSERT OR IGNORE INTO streams(stream_id, login, started_ts, ended_ts, last_seen, game, title, \
             peak_viewers, games) VALUES (?1, ?2, ?3, ?4, ?4, '', ?5, 0, '{}')",
            params![stream_id, login, started_ts, ended_ts, title],
        )?;
        Ok(())
    }

    /// `{логин: времена начала эфиров с since_ts}`: сырьё для прогноза.
    pub fn list_stream_starts(&self, since_ts: f64) -> Result<HashMap<String, Vec<f64>>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT login, started_ts FROM streams WHERE started_ts >= ?1 ORDER BY started_ts")?;
        let rows = stmt.query_map([since_ts], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))?;
        let mut starts: HashMap<String, Vec<f64>> = HashMap::new();
        for row in rows {
            let (login, ts) = row?;
            starts.entry(login).or_default().push(ts);
        }
        Ok(starts)
    }

    // ---------- расписания ----------
    pub fn save_schedule(&self, login: &str, fetched_ts: f64, segments: &[ScheduleSegment]) -> Result<()> {
        let json = serde_json::to_string(segments).unwrap_or_else(|_| "[]".into());
        self.conn()?.execute(
            "INSERT OR REPLACE INTO schedules(login, fetched_ts, segments) VALUES (?1, ?2, ?3)",
            params![login, fetched_ts, json],
        )?;
        Ok(())
    }

    pub fn list_schedules(&self) -> Result<HashMap<String, StoredSchedule>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT login, fetched_ts, segments FROM schedules")?;
        let rows = stmt.query_map([], |r| {
            let json: String = r.get::<_, Option<String>>(2)?.unwrap_or_else(|| "[]".into());
            Ok((
                r.get::<_, String>(0)?,
                StoredSchedule { fetched_ts: r.get(1)?, segments: serde_json::from_str(&json).unwrap_or_default() },
            ))
        })?;
        rows.collect()
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// База во временной папке, которая удаляется вместе со значением.
    pub struct TempRepo {
        pub repo: Repository,
        dir: PathBuf,
    }

    impl TempRepo {
        pub fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!("tt-test-{}-{}", std::process::id(), n));
            std::fs::create_dir_all(&dir).unwrap();
            TempRepo { repo: Repository::open(dir.join("t.db")).unwrap(), dir }
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::TempRepo;
    use super::*;

    #[test]
    fn add_and_duplicates() {
        let t = TempRepo::new();
        assert!(t.repo.add_streamer("shroud", "Shroud", DEFAULT_GROUP, "").unwrap());
        assert!(!t.repo.add_streamer("shroud", "Shroud", DEFAULT_GROUP, "").unwrap());
        let logins: Vec<String> = t.repo.list_streamers().unwrap().into_iter().map(|s| s.login).collect();
        assert_eq!(logins, ["shroud"]);
    }

    #[test]
    fn groups_and_delete_moves_streamers() {
        let t = TempRepo::new();
        t.repo.add_group("Друзья").unwrap();
        t.repo.add_streamer("a", "", "Друзья", "").unwrap();
        t.repo.delete_group("Друзья").unwrap();
        assert!(!t.repo.list_groups().unwrap().contains(&"Друзья".to_string()));
        assert_eq!(t.repo.list_streamers().unwrap()[0].group, DEFAULT_GROUP);
        t.repo.delete_group(DEFAULT_GROUP).unwrap();
        assert!(t.repo.list_groups().unwrap().contains(&DEFAULT_GROUP.to_string()));
    }

    #[test]
    fn settings_and_event_overrides() {
        let t = TempRepo::new();
        assert_eq!(t.repo.get_int("poll_interval", 60), 60);
        t.repo.set_setting("poll_interval", "90").unwrap();
        assert_eq!(t.repo.get_int("poll_interval", 60), 90);
        t.repo.set_bool("notifications", false).unwrap();
        assert!(!t.repo.get_bool("notifications", true));

        t.repo.add_streamer("a", "", DEFAULT_GROUP, "").unwrap();
        t.repo.set_event_override("a", EventKind::Title, Some(true)).unwrap();
        t.repo.set_event_override("a", EventKind::Live, Some(false)).unwrap();
        let o = t.repo.get_streamer("a").unwrap().unwrap().events;
        assert_eq!((o.live, o.end, o.game, o.title), (Some(false), None, None, Some(true)));
        t.repo.set_event_override("a", EventKind::Title, None).unwrap();
        assert_eq!(t.repo.get_streamer("a").unwrap().unwrap().events.title, None);
    }

    #[test]
    fn profile_does_not_wipe_with_empty_values() {
        let t = TempRepo::new();
        t.repo.add_streamer("a", "A", DEFAULT_GROUP, "").unwrap();
        t.repo.set_profile("a", "A", "http://img", "777").unwrap();
        t.repo.set_profile("a", "", "", "").unwrap();
        t.repo.mark_vod_synced("a", 99.0).unwrap();
        t.repo.mark_live(&["a".into(), "nobody".into()], 123.0).unwrap();
        let s = t.repo.get_streamer("a").unwrap().unwrap();
        assert_eq!((s.display_name.as_str(), s.avatar_url.as_str(), s.twitch_id.as_str()), ("A", "http://img", "777"));
        assert_eq!((s.vod_synced, s.last_live), (Some(99.0), Some(123.0)));
    }

    #[test]
    fn events_feed_limit_and_clear() {
        let t = TempRepo::new();
        let mut live = NewEvent::new(EventKind::Live, "a", 100.0);
        live.name = "A".into();
        t.repo.add_event(&live).unwrap();
        let mut end = NewEvent::new(EventKind::End, "a", 200.0);
        end.duration = Some(60.0);
        t.repo.add_event(&end).unwrap();
        let kinds: Vec<String> = t.repo.list_events(10).unwrap().into_iter().map(|e| e.kind).collect();
        assert_eq!(kinds, ["end", "live"]);

        for i in 0..(EVENT_LIMIT + 50) {
            t.repo.add_event(&NewEvent::new(EventKind::Live, &format!("u{i}"), i as f64)).unwrap();
        }
        assert_eq!(t.repo.list_events(5000).unwrap().len() as i64, EVENT_LIMIT);
        t.repo.clear_events().unwrap();
        assert!(t.repo.list_events(10).unwrap().is_empty());
    }

    fn update<'a>(id: &'a str, now: f64, game: &'a str, viewers: i64, seen: f64) -> StreamUpdate<'a> {
        StreamUpdate { stream_id: id, login: "a", started_ts: 1000.0, now, game, title: "T", viewers, seen_seconds: seen }
    }

    #[test]
    fn stream_lifecycle() {
        let t = TempRepo::new();
        t.repo.upsert_stream(&update("s1", 1060.0, "Chat", 10, 0.0)).unwrap();
        t.repo.upsert_stream(&update("s1", 1120.0, "Chat", 30, 60.0)).unwrap();
        t.repo.upsert_stream(&update("s1", 1180.0, "GTA", 20, 60.0)).unwrap();
        let row = t.repo.get_stream("s1").unwrap().unwrap();
        assert_eq!(row.peak_viewers, 30);
        assert_eq!(row.games.get("Chat"), Some(&60.0));
        assert_eq!(row.games.get("GTA"), Some(&60.0));
        assert_eq!(row.ended_ts, None);

        t.repo.close_open_streams(&["other".to_string()].into()).unwrap();
        assert_eq!(t.repo.get_stream("s1").unwrap().unwrap().ended_ts, Some(1180.0));

        t.repo.upsert_stream(&update("s1", 1240.0, "GTA", 20, 60.0)).unwrap(); // вернулся после сбоя
        assert_eq!(t.repo.get_stream("s1").unwrap().unwrap().ended_ts, None);
        t.repo.close_open_streams(&["s1".to_string()].into()).unwrap();
        assert_eq!(t.repo.get_stream("s1").unwrap().unwrap().ended_ts, None);
    }

    #[test]
    fn import_does_not_overwrite_detected_stream_and_starts_are_grouped() {
        let t = TempRepo::new();
        t.repo.upsert_stream(&update("s1", 1100.0, "Chess", 50, 60.0)).unwrap();
        t.repo.import_past_stream("s1", "a", 1000.0, 2000.0, "vod title").unwrap();
        let row = t.repo.get_stream("s1").unwrap().unwrap();
        assert_eq!((row.game.as_str(), row.title.as_str(), row.peak_viewers), ("Chess", "T", 50));

        t.repo.import_past_stream("s2", "a", 5000.0, 6000.0, "vod").unwrap();
        t.repo.import_past_stream("s3", "b", 5500.0, 6000.0, "vod").unwrap();
        let starts = t.repo.list_stream_starts(2000.0).unwrap();
        assert_eq!(starts["a"], [5000.0]);
        assert_eq!(starts["b"], [5500.0]);
        // s1 ещё считается идущим (не закрыт), поэтому попадает в любой период; после закрытия остаётся только s2
        t.repo.close_open_streams(&HashSet::new()).unwrap();
        let ids: Vec<String> = t.repo.list_streams_since(4000.0).unwrap().into_iter().map(|r| r.stream_id).collect();
        assert!(ids.contains(&"s2".to_string()) && !ids.contains(&"s1".to_string()));
    }

    #[test]
    fn schedules_roundtrip() {
        let t = TempRepo::new();
        let seg = ScheduleSegment { start_ts: 5.0, title: "x".into(), ..Default::default() };
        t.repo.save_schedule("a", 123.0, &[seg.clone()]).unwrap();
        t.repo.save_schedule("b", 124.0, &[]).unwrap();
        let all = t.repo.list_schedules().unwrap();
        assert_eq!(all["a"].segments, [seg]);
        assert!(all["b"].segments.is_empty());
    }

    #[test]
    fn old_database_is_migrated_in_place() {
        let dir = std::env::temp_dir().join(format!("tt-old-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE groups (name TEXT PRIMARY KEY);
                 CREATE TABLE streamers (login TEXT PRIMARY KEY, display_name TEXT NOT NULL DEFAULT '',
                    group_name TEXT NOT NULL DEFAULT 'Общее', favorite INTEGER NOT NULL DEFAULT 0);
                 INSERT INTO streamers(login, favorite) VALUES ('shroud', 1);",
            )
            .unwrap();
        }
        let repo = Repository::open(&path).unwrap();
        let s = repo.get_streamer("shroud").unwrap().unwrap();
        assert!(s.favorite);
        assert_eq!((s.avatar_url.as_str(), s.last_live, s.twitch_id.as_str()), ("", None, ""));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
