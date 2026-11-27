use serde::{Deserialize, Deserializer, Serialize};

fn nullable<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// Идущая трансляция в ответе Get Streams. Лишние поля Twitch игнорируются.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LiveStream {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub user_id: String,
    #[serde(deserialize_with = "nullable")]
    pub user_login: String,
    #[serde(deserialize_with = "nullable")]
    pub user_name: String,
    #[serde(deserialize_with = "nullable")]
    pub game_id: String,
    #[serde(deserialize_with = "nullable")]
    pub game_name: String,
    #[serde(deserialize_with = "nullable")]
    pub title: String,
    #[serde(deserialize_with = "nullable")]
    pub viewer_count: u64,
    #[serde(deserialize_with = "nullable")]
    pub started_at: String,
    #[serde(deserialize_with = "nullable")]
    pub language: String,
    #[serde(deserialize_with = "nullable")]
    pub tags: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    pub thumbnail_url: String,
    #[serde(rename = "type", deserialize_with = "nullable")]
    pub kind: String,
}

impl LiveStream {
    /// Уникальный ключ эфира: id трансляции (если его нет, то логин и время старта).
    pub fn key(&self, login: &str) -> String {
        if self.id.is_empty() {
            format!("{}:{}", login, self.started_at)
        } else {
            self.id.clone()
        }
    }

    /// По чему определяем смену категории: id игры, а если его нет, название.
    pub fn game_key(&self) -> &str {
        if self.game_id.is_empty() {
            &self.game_name
        } else {
            &self.game_id
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_twitch_payload_with_nulls_and_extra_fields() {
        let json = r#"{"id":"42","user_login":"amy","user_name":"Amy","game_id":null,"game_name":"Chess",
            "title":"Hi","viewer_count":1500,"started_at":"2026-10-10T18:00:00Z","tags":null,"type":"live","extra":1}"#;
        let s: LiveStream = serde_json::from_str(json).unwrap();
        assert_eq!((s.id.as_str(), s.viewer_count, s.kind.as_str()), ("42", 1500, "live"));
        assert!(s.tags.is_empty());
        assert_eq!(s.game_key(), "Chess");
    }

    #[test]
    fn key_falls_back_to_login_and_start() {
        let s = LiveStream { started_at: "T".into(), ..Default::default() };
        assert_eq!(s.key("amy"), "amy:T");
    }
}
