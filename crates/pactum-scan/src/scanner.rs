//! Сам сканер: обход списка, дедупликация, вежливость, отчёт.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::candidates::{Candidate, CandidateQueue, CandidateStatus};
use crate::fetch::{FetchOutcome, Fetcher};
use crate::source::{Source, SourceSpec, Watchlist};

/// Состояние одного источника между прогонами.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    #[serde(default)]
    pub last_run: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub runs: u64,
    #[serde(default)]
    pub changes: u64,
}

/// Состояние сканера: файл JSON, переживает рестарт.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanState {
    pub sources: BTreeMap<String, SourceState>,
}

impl ScanState {
    pub fn load(path: &std::path::Path) -> ScanState {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

/// Настройки прогона.
#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    /// Публиковать ли кандидатов сразу (иначе — только очередь).
    pub publish: bool,
    /// Показывать ли содержимое в отчёте.
    pub verbose: bool,
}

/// Отчёт прогона — честный, по каждому источнику.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ScanReport {
    pub checked: Vec<String>,
    pub changed: Vec<String>,
    pub skipped_interval: Vec<String>,
    pub skipped_volatile: Vec<String>,
    pub failed: Vec<(String, String)>,
}

impl ScanReport {
    pub fn summary(&self) -> String {
        format!(
            "проверено {}, изменилось {}, пропущено (интервал) {}, пропущено (изменчивые) {}, ошибок {}",
            self.checked.len(),
            self.changed.len(),
            self.skipped_interval.len(),
            self.skipped_volatile.len(),
            self.failed.len()
        )
    }
}

/// Сканер: чистая логика поверх Fetcher — легко тестировать.
pub struct Scanner<'a> {
    pub fetcher: &'a dyn Fetcher,
    /// Внутренние лимиты по хостам (только для url-источников).
    host_last_fetch: std::cell::RefCell<BTreeMap<String, u64>>,
}

/// Минимальная вежливость: не чаще раза в секунду на хост.
const HOST_MIN_GAP_MS: u64 = 1000;

impl<'a> Scanner<'a> {
    pub fn new(fetcher: &'a dyn Fetcher) -> Scanner<'a> {
        Scanner {
            fetcher,
            host_last_fetch: std::cell::RefCell::new(BTreeMap::new()),
        }
    }

    /// Один прогон по всему списку. Ошибки одного источника не валят остальные.
    pub fn run_once(
        &mut self,
        watchlist: &Watchlist,
        state: &mut ScanState,
        queue: &CandidateQueue,
        options: &ScanOptions,
    ) -> ScanReport {
        let now = pactum_core::time::now();
        let mut report = ScanReport::default();
        for source in &watchlist.sources {
            self.scan_source(source, state, queue, options, now, &mut report);
        }
        if options.publish {
            // публикация — отдельно и только кандидатов со статусом new;
            // HTTP-часть делает CLI/node, здесь только отметка в отчёте
            if let Ok(pending) = queue.by_status(CandidateStatus::New) {
                for candidate in pending {
                    report
                        .changed
                        .push(format!("готов к публикации: {}", candidate.source));
                }
            }
        }
        report
    }

    #[allow(clippy::too_many_arguments)]
    fn scan_source(
        &mut self,
        source: &Source,
        state: &mut ScanState,
        queue: &CandidateQueue,
        _options: &ScanOptions,
        now: u64,
        report: &mut ScanReport,
    ) {
        let entry = state.sources.entry(source.name.clone()).or_default();
        // вежливость: интервал источника
        if entry.last_run > 0 && now.saturating_sub(entry.last_run) < source.interval_s {
            report.skipped_interval.push(source.name.clone());
            return;
        }
        // вежливость: пауза по хосту
        if let Some(host) = source.host() {
            self.wait_for_host(&host);
        }

        let outcome = self.fetcher.fetch(
            source,
            entry.etag.as_deref(),
            entry.last_modified.as_deref(),
        );
        let entry = state.sources.entry(source.name.clone()).or_default();
        entry.last_run = now;
        entry.runs += 1;
        report.checked.push(source.name.clone());

        match outcome {
            Ok(FetchOutcome::NotModified) => {
                entry.last_error = None;
            }
            Ok(FetchOutcome::Content {
                body,
                etag,
                last_modified,
            }) => {
                let new_hash = pactum_core::hash::sha256_hex(&body);
                let changed = entry.last_hash.as_deref() != Some(new_hash.as_str());
                entry.etag = etag;
                entry.last_modified = last_modified;
                entry.last_error = None;
                if changed {
                    let is_cmd = matches!(source.spec, SourceSpec::Cmd { .. });
                    if is_cmd && !source.allow_volatile {
                        // честный пропуск: вывод команд меняется сам по себе
                        report.skipped_volatile.push(source.name.clone());
                        entry.last_hash = Some(new_hash);
                        return;
                    }
                    let old = entry.last_hash.clone();
                    let candidate = Candidate::new(
                        &source.name,
                        source.topic_name(),
                        old,
                        new_hash.clone(),
                        &body,
                        now,
                    );
                    if queue.append(&candidate).is_ok() {
                        report.changed.push(source.name.clone());
                    } else {
                        report
                            .failed
                            .push((source.name.clone(), "очередь не приняла кандидата".into()));
                    }
                    entry.changes += 1;
                    entry.last_hash = Some(new_hash);
                }
            }
            Err(err) => {
                let reason = err.to_string();
                report.failed.push((source.name.clone(), reason.clone()));
                entry.last_error = Some(reason);
            }
        }
    }

    fn wait_for_host(&self, host: &str) {
        let mut map = self.host_last_fetch.borrow_mut();
        let now_ms = pactum_core::time::now_millis() as u64;
        let elapsed = map
            .get(host)
            .map(|last_ms| now_ms.saturating_sub(*last_ms))
            .unwrap_or(u64::MAX);
        if elapsed < HOST_MIN_GAP_MS {
            std::thread::sleep(std::time::Duration::from_millis(HOST_MIN_GAP_MS - elapsed));
        }
        map.insert(host.to_string(), now_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::MockFetcher;
    use crate::source::{SourceSpec, Watchlist};

    fn list(sources: &[(&str, SourceSpec)]) -> Watchlist {
        Watchlist {
            sources: sources
                .iter()
                .map(|(name, spec)| Source {
                    name: name.to_string(),
                    spec: spec.clone(),
                    interval_s: 0,
                    allow_volatile: false,
                    topic: None,
                })
                .collect(),
        }
    }

    fn queue() -> CandidateQueue {
        let dir = std::env::temp_dir().join(format!(
            "pactum-scan-s-{}-{}",
            std::process::id(),
            pactum_core::id::random_hex(4)
        ));
        CandidateQueue::open(dir.join("q.jsonl")).unwrap()
    }

    fn cleanup(path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            std::fs::remove_dir_all(parent).ok();
        }
    }

    #[test]
    fn first_run_records_hash_second_run_is_quiet() {
        let mut mock = MockFetcher::new();
        mock.set("doc", MockFetcher::content("doc", "версия 1"));
        let watchlist = list(&[(
            "doc",
            SourceSpec::Url {
                url: "https://x/doc".into(),
            },
        )]);
        let mut state = ScanState::default();
        let q = queue();
        let mut scanner = Scanner::new(&mock);
        let report = scanner.run_once(&watchlist, &mut state, &q, &ScanOptions::default());
        assert_eq!(report.changed.len(), 1, "первое появление — кандидат");
        assert_eq!(
            report.summary(),
            "проверено 1, изменилось 1, пропущено (интервал) 0, пропущено (изменчивые) 0, ошибок 0"
        );
        // повторный прогон: то же содержимое — тишина
        let report = scanner.run_once(&watchlist, &mut state, &q, &ScanOptions::default());
        assert_eq!(report.changed.len(), 0);
        assert_eq!(
            state.sources["doc"].last_hash,
            Some(pactum_core::hash::sha256_text("версия 1"))
        );
        cleanup(q.path());
    }

    #[test]
    fn change_creates_candidate_with_sample() {
        let mut mock = MockFetcher::new();
        mock.set("doc", MockFetcher::content("doc", "версия 2"));
        let watchlist = list(&[(
            "doc",
            SourceSpec::Url {
                url: "https://x/doc".into(),
            },
        )]);
        let mut state = ScanState::default();
        state.sources.insert(
            "doc".into(),
            SourceState {
                last_hash: Some("старый-хеш".into()),
                ..Default::default()
            },
        );
        let q = queue();
        let mut scanner = Scanner::new(&mock);
        scanner.run_once(&watchlist, &mut state, &q, &ScanOptions::default());
        let candidates = q.by_status(CandidateStatus::New).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].old_hash.as_deref(), Some("старый-хеш"));
        assert_eq!(candidates[0].sample, "версия 2");
        cleanup(q.path());
    }

    #[test]
    fn not_modified_and_failures_are_visible() {
        let mut mock = MockFetcher::new();
        mock.set("calm", MockFetcher::not_modified());
        mock.set("dead", MockFetcher::failed("HTTP 500"));
        let watchlist = list(&[
            (
                "calm",
                SourceSpec::Url {
                    url: "https://x/calm".into(),
                },
            ),
            (
                "dead",
                SourceSpec::Url {
                    url: "https://x/dead".into(),
                },
            ),
        ]);
        let mut state = ScanState::default();
        state.sources.insert(
            "calm".into(),
            SourceState {
                last_hash: Some("h".into()),
                ..Default::default()
            },
        );
        let q = queue();
        let mut scanner = Scanner::new(&mock);
        let report = scanner.run_once(&watchlist, &mut state, &q, &ScanOptions::default());
        assert_eq!(report.changed.len(), 0);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].0, "dead");
        assert_eq!(report.failed[0].1, "сеть: HTTP 500");
        assert_eq!(
            state.sources["dead"].last_error.as_deref(),
            Some("сеть: HTTP 500")
        );
        // а живой источник работает дальше
        assert!(state.sources["calm"].last_error.is_none());
        cleanup(q.path());
    }

    #[test]
    fn volatile_cmd_source_is_skipped_without_flag() {
        let mut mock = MockFetcher::new();
        mock.set(
            "df",
            MockFetcher::content("df", "Filesystem 1K-blocks Used 42%"),
        );
        let watchlist = list(&[(
            "df",
            SourceSpec::Cmd {
                cmd: "df -h /".into(),
            },
        )]);
        let mut state = ScanState::default();
        state.sources.insert(
            "df".into(),
            SourceState {
                last_hash: Some("другой".into()),
                ..Default::default()
            },
        );
        let q = queue();
        let mut scanner = Scanner::new(&mock);
        let report = scanner.run_once(&watchlist, &mut state, &q, &ScanOptions::default());
        assert_eq!(report.changed.len(), 0);
        assert_eq!(report.skipped_volatile, vec!["df".to_string()]);
        assert!(q.all().unwrap().is_empty());
        cleanup(q.path());
    }

    #[test]
    fn volatile_cmd_source_with_flag_creates_candidate() {
        let mut mock = MockFetcher::new();
        mock.set(
            "df",
            MockFetcher::content("df", "Filesystem 1K-blocks Used 99%"),
        );
        let mut watchlist = list(&[(
            "df",
            SourceSpec::Cmd {
                cmd: "df -h /".into(),
            },
        )]);
        watchlist.sources[0].allow_volatile = true;
        let mut state = ScanState::default();
        state.sources.insert(
            "df".into(),
            SourceState {
                last_hash: Some("другой".into()),
                ..Default::default()
            },
        );
        let q = queue();
        let mut scanner = Scanner::new(&mock);
        let report = scanner.run_once(&watchlist, &mut state, &q, &ScanOptions::default());
        assert_eq!(report.changed.len(), 1);
        cleanup(q.path());
    }

    #[test]
    fn interval_respected() {
        let mut mock = MockFetcher::new();
        mock.set("doc", MockFetcher::content("doc", "x"));
        let mut watchlist = list(&[(
            "doc",
            SourceSpec::Url {
                url: "https://x/doc".into(),
            },
        )]);
        watchlist.sources[0].interval_s = 3600;
        let mut state = ScanState::default();
        let q = queue();
        let mut scanner = Scanner::new(&mock);
        let report = scanner.run_once(&watchlist, &mut state, &q, &ScanOptions::default());
        assert_eq!(report.checked.len(), 1);
        let report = scanner.run_once(&watchlist, &mut state, &q, &ScanOptions::default());
        assert_eq!(report.skipped_interval, vec!["doc".to_string()]);
        cleanup(q.path());
    }

    #[test]
    fn state_saves_and_loads() {
        let dir = std::env::temp_dir().join(format!("pactum-scan-st-{}", std::process::id()));
        let path = dir.join("scan.json");
        let mut state = ScanState::default();
        state.sources.insert(
            "doc".into(),
            SourceState {
                last_hash: Some("abc".into()),
                runs: 7,
                ..Default::default()
            },
        );
        state.save(&path).unwrap();
        let back = ScanState::load(&path);
        assert_eq!(back.sources["doc"].runs, 7);
        assert_eq!(back.sources["doc"].last_hash.as_deref(), Some("abc"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
