//! Ядро Twitch Tracker без интерфейса: всё, что можно проверить тестами.
//!
//! Формат базы SQLite совпадает с Python-версией, поэтому Rust-версия открывает
//! ту же папку `%APPDATA%\TwitchTracker` и подхватывает каналы, настройки и историю.

pub mod backup;
pub mod events;
pub mod history_import;
pub mod models;
pub mod policy;
pub mod prediction;
pub mod quiet_digest;
pub mod repository;
pub mod stats;
pub mod stream;
