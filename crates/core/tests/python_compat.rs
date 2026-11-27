//! Совместимость с Python-версией: Rust читает базу, созданную ею, а Python читает базу, созданную Rust.

use std::path::PathBuf;

use tracker_core::models::{EventKind, DEFAULT_GROUP};
use tracker_core::policy::NotifySettings;
use tracker_core::prediction::ScheduleSegment;
use tracker_core::repository::{NewEvent, Repository, StreamUpdate};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/python_v06.db")
}

#[test]
fn reads_database_written_by_python() {
    // работаем с копией: открытие базы выполняет миграции и не должно менять фикстуру в репозитории
    let dir = std::env::temp_dir().join(format!("tt-compat-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let copy = dir.join("copy.db");
    std::fs::copy(fixture(), &copy).unwrap();
    let repo = Repository::open(&copy).unwrap();

    let streamers = repo.list_streamers().unwrap();
    assert_eq!(streamers.len(), 2);
    let amy = repo.get_streamer("amy").unwrap().unwrap();
    assert_eq!((amy.display_name.as_str(), amy.group.as_str(), amy.favorite), ("Amy", "Друзья", true));
    assert_eq!((amy.avatar_url.as_str(), amy.twitch_id.as_str()), ("http://img/amy", "123"));
    assert_eq!((amy.last_live, amy.vod_synced), (Some(1700000000.5), Some(1700000500.0)));
    assert_eq!((amy.events.title, amy.events.end, amy.events.live, amy.events.game), (Some(true), Some(false), None, None));
    assert_eq!(repo.get_streamer("bob").unwrap().unwrap().group, DEFAULT_GROUP);
    assert_eq!(repo.list_groups().unwrap(), [DEFAULT_GROUP.to_string(), "Друзья".to_string()]);

    let settings = NotifySettings::load(&repo);
    assert!(settings.favorites_only);
    assert_eq!(settings.quiet_start.to_string(), "22:30:00");
    assert_eq!(repo.get_int("poll_interval", 60), 15);
    assert_eq!(repo.get_setting("language").unwrap().as_deref(), Some("en"));

    let events = repo.list_events(10).unwrap();
    assert_eq!(events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), ["end", "live"]);
    assert_eq!((events[0].duration, events[1].title.as_str()), (Some(3600.0), "Привет"));

    let stream = repo.get_stream("s1").unwrap().unwrap();
    assert_eq!((stream.peak_viewers, stream.ended_ts), (30, Some(1700000120.0)));
    assert_eq!(stream.games.get("Chess"), Some(&60.0));

    let schedules = repo.list_schedules().unwrap();
    let seg = &schedules["amy"].segments[0];
    assert_eq!((seg.title.as_str(), seg.start_ts, seg.canceled), ("Пятница", 1700100000.0, false));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Пишет базу, если задана переменная `TT_COMPAT_OUT`; её потом открывает Python (проверяется вручную и в CI).
#[test]
fn writes_database_python_can_read() {
    let Ok(path) = std::env::var("TT_COMPAT_OUT") else { return };
    let _ = std::fs::remove_file(&path);
    let repo = Repository::open(&path).unwrap();
    repo.add_group("Из Rust").unwrap();
    repo.add_streamer("zed", "Zed", "Из Rust", "http://img/zed").unwrap();
    repo.set_favorite("zed", true).unwrap();
    repo.set_event_override("zed", EventKind::Game, Some(true)).unwrap();
    repo.set_profile("zed", "", "", "999").unwrap();
    repo.set_setting("poll_interval", "20").unwrap();
    let mut e = NewEvent::new(EventKind::Live, "zed", 1700000000.0);
    e.name = "Zed".into();
    e.game = "Go".into();
    e.title = "Партия".into();
    repo.add_event(&e).unwrap();
    repo.upsert_stream(&StreamUpdate {
        stream_id: "r1",
        login: "zed",
        started_ts: 1700000000.0,
        now: 1700000060.0,
        game: "Go",
        title: "Партия",
        viewers: 7,
        seen_seconds: 60.0,
    })
    .unwrap();
    repo.save_schedule("zed", 1700000000.0, &[ScheduleSegment { start_ts: 1700100000.0, title: "Вторник".into(), ..Default::default() }]).unwrap();
}
