//! Наблюдаемый список: источники и их параметры.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum WatchlistError {
    #[error("источник «{0}» должен задать ровно один из url/path/cmd")]
    AmbiguousSpec(String),
    #[error("источник «{0}» без имени")]
    NoName(String),
}

pub type WatchResult<T> = Result<T, WatchlistError>;

/// Чем является источник. Ровно один вариант.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SourceSpec {
    Url { url: String },
    File { path: String },
    Cmd { cmd: String },
}

impl SourceSpec {
    pub fn describe(&self) -> String {
        match self {
            SourceSpec::Url { url } => format!("url:{url}"),
            SourceSpec::File { path } => format!("file:{path}"),
            SourceSpec::Cmd { cmd } => format!("cmd:{cmd}"),
        }
    }
}

/// Один источник наблюдения.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub name: String,
    #[serde(flatten)]
    pub spec: SourceSpec,
    /// Минимальный интервал между проверками (вежливость), секунд.
    #[serde(default = "default_interval")]
    pub interval_s: u64,
    /// Источник, меняющийся при каждом чтении, — только с явного разрешения.
    #[serde(default)]
    pub allow_volatile: bool,
    /// Топик хаба для публикации (по умолчанию — имя источника).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
}

fn default_interval() -> u64 {
    300
}

impl Source {
    pub fn topic_name(&self) -> &str {
        self.topic.as_deref().unwrap_or(&self.name)
    }

    /// Хост для лимита запросов (только url).
    pub fn host(&self) -> Option<String> {
        match &self.spec {
            SourceSpec::Url { url } => {
                let rest = url
                    .strip_prefix("https://")
                    .or_else(|| url.strip_prefix("http://"))?;
                let host = rest.split(['/', ':']).next().unwrap_or(rest);
                Some(host.to_string())
            }
            _ => None,
        }
    }
}

/// Наблюдаемый список. Файл JSON, читается и правится руками.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Watchlist {
    pub sources: Vec<Source>,
}

impl Watchlist {
    pub fn load(path: &std::path::Path) -> WatchResult<Watchlist> {
        let text = std::fs::read_to_string(path)
            .map_err(|err| WatchlistError::NoName(format!("не прочитать список: {err}")))?;
        Watchlist::parse(&text)
    }

    pub fn parse(text: &str) -> WatchResult<Watchlist> {
        let list: Watchlist = serde_json::from_str(text)
            .map_err(|err| WatchlistError::NoName(format!("список не разобран: {err}")))?;
        list.validate()?;
        Ok(list)
    }

    pub fn validate(&self) -> WatchResult<()> {
        for source in &self.sources {
            if source.name.trim().is_empty() {
                return Err(WatchlistError::NoName(source.spec.describe()));
            }
            // untagged serde выберет первый подходящий вариант; проверим честно:
            let spec_json = serde_json::to_value(&source.spec)
                .map_err(|_err| WatchlistError::AmbiguousSpec(source.name.clone()))?;
            let keys: Vec<&str> = spec_json
                .as_object()
                .map(|o| o.keys().map(String::as_str).collect())
                .unwrap_or_default();
            if keys.len() != 1 {
                return Err(WatchlistError::AmbiguousSpec(source.name.clone()));
            }
        }
        Ok(())
    }

    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_all_kinds() {
        let text = r#"{
            "sources": [
                {"name": "blog", "url": "https://blog.rust-lang.org/feed.xml", "interval_s": 600},
                {"name": "report", "path": "reports/daily.json"},
                {"name": "disk", "cmd": "df -h /", "interval_s": 3600, "allow_volatile": true}
            ]
        }"#;
        let list = Watchlist::parse(text).unwrap();
        assert_eq!(list.sources.len(), 3);
        assert_eq!(
            list.sources[0].spec,
            SourceSpec::Url {
                url: "https://blog.rust-lang.org/feed.xml".into()
            }
        );
        assert_eq!(list.sources[1].interval_s, 300);
        assert!(list.sources[2].allow_volatile);
        assert_eq!(list.sources[2].spec.describe(), "cmd:df -h /");
    }

    #[test]
    fn rejects_empty_name() {
        let text = r#"{"sources": [{"url": "https://x"}]}"#;
        assert!(Watchlist::parse(text).is_err());
    }

    #[test]
    fn host_extracted_from_url() {
        let list =
            Watchlist::parse(r#"{"sources":[{"name":"x","url":"https://example.com:8443/a/b"}]}"#)
                .unwrap();
        assert_eq!(list.sources[0].host().as_deref(), Some("example.com"));
        assert_eq!(list.sources[0].topic_name(), "x");
    }

    #[test]
    fn topic_defaults_to_name() {
        let list = Watchlist::parse(r#"{"sources":[{"name":"файл","path":"/tmp/x"}]}"#).unwrap();
        assert_eq!(list.sources[0].topic_name(), "файл");
    }
}
