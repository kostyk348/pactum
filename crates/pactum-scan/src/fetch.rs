//! Доставка содержимого источника: трейт для тестов, HTTP для дела.

use crate::source::{Source, SourceSpec};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Clone)]
pub enum FetchError {
    #[error("сеть: {0}")]
    Network(String),
    #[error("io: {0}")]
    Io(String),
}

pub type FetchResult<T> = Result<T, FetchError>;

/// Итог одного чтения источника.
#[derive(Debug, Clone, PartialEq)]
pub enum FetchOutcome {
    /// Содержимое получено (новое или изменённое) + условия для следующего раза.
    Content {
        body: Vec<u8>,
        etag: Option<String>,
        last_modified: Option<String>,
    },
    /// 304: содержимое не менялось — хеш и так известен.
    NotModified,
}

/// Кто читает источники. HTTP-реализация — для дела, mock — для тестов.
pub trait Fetcher: Send + Sync {
    fn fetch(
        &self,
        source: &Source,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> FetchResult<FetchOutcome>;
}

/// HTTP-читалка: условный GET, вежливые тайм-ауты.
#[derive(Default)]
pub struct HttpFetcher;

impl Fetcher for HttpFetcher {
    fn fetch(
        &self,
        source: &Source,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> FetchResult<FetchOutcome> {
        let url = match &source.spec {
            SourceSpec::Url { url } => url.clone(),
            other => {
                return Err(FetchError::Network(format!(
                    "HTTP-читалка умеет только url, получено: {other:?}"
                )))
            }
        };
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(10))
            .timeout_read(std::time::Duration::from_secs(30))
            .build();
        let mut request = agent.get(&url);
        if let Some(tag) = etag {
            request = request.set("If-None-Match", tag);
        }
        if let Some(since) = last_modified {
            request = request.set("If-Modified-Since", since);
        }
        match request.call() {
            Ok(resp) => {
                let etag = resp.header("ETag").map(str::to_string);
                let modified = resp.header("Last-Modified").map(str::to_string);
                let mut body = Vec::new();
                use std::io::Read;
                resp.into_reader()
                    .take(16 * 1024 * 1024)
                    .read_to_end(&mut body)
                    .map_err(|err| FetchError::Network(err.to_string()))?;
                Ok(FetchOutcome::Content {
                    body,
                    etag,
                    last_modified: modified,
                })
            }
            Err(ureq::Error::Status(304, _)) => Ok(FetchOutcome::NotModified),
            Err(ureq::Error::Status(code, _)) => Err(FetchError::Network(format!("HTTP {code}"))),
            Err(err) => Err(FetchError::Network(err.to_string())),
        }
    }
}

/// Mock для тестов: поведение задаётся таблицей имя → результат.
pub struct MockFetcher {
    pub responses: std::collections::HashMap<String, FetchResult<FetchOutcome>>,
}

impl MockFetcher {
    pub fn new() -> MockFetcher {
        MockFetcher {
            responses: std::collections::HashMap::new(),
        }
    }

    pub fn set(&mut self, name: &str, outcome: FetchResult<FetchOutcome>) {
        self.responses.insert(name.to_string(), outcome);
    }

    pub fn content(_name: &str, body: &str) -> FetchResult<FetchOutcome> {
        Ok(FetchOutcome::Content {
            body: body.as_bytes().to_vec(),
            etag: None,
            last_modified: None,
        })
    }

    pub fn not_modified() -> FetchResult<FetchOutcome> {
        Ok(FetchOutcome::NotModified)
    }

    pub fn failed(reason: &str) -> FetchResult<FetchOutcome> {
        Err(FetchError::Network(reason.to_string()))
    }
}

impl Default for MockFetcher {
    fn default() -> Self {
        MockFetcher::new()
    }
}

impl Fetcher for MockFetcher {
    fn fetch(
        &self,
        source: &Source,
        _etag: Option<&str>,
        _last_modified: Option<&str>,
    ) -> FetchResult<FetchOutcome> {
        self.responses
            .get(&source.name)
            .cloned()
            .unwrap_or_else(MockFetcher::not_modified)
    }
}
