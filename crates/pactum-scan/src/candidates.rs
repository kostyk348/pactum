//! Очередь кандидатов: сбор отделён от доставки, решение — за человеком.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum QueueError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type QueueResult<T> = Result<T, QueueError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CandidateStatus {
    /// Найдено, решение не принято.
    New,
    /// Человек подтвердил: публиковать.
    Accepted,
    /// Человек отклонил — с причиной.
    Rejected,
}

/// Один кандидат: «источник X изменился».
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub source: String,
    pub topic: String,
    pub detected_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_hash: Option<String>,
    pub new_hash: String,
    /// Первые строки нового содержимого — чтобы решение принималось глазами.
    pub sample: String,
    pub status: CandidateStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Candidate {
    const SAMPLE_LINES: usize = 5;

    pub fn new(
        source: &str,
        topic: &str,
        old_hash: Option<String>,
        new_hash: String,
        content: &[u8],
        at: u64,
    ) -> Candidate {
        let text = String::from_utf8_lossy(content);
        let sample: Vec<&str> = text.lines().take(Self::SAMPLE_LINES).collect();
        Candidate {
            id: pactum_core::id::new_object_id("cand"),
            source: source.to_string(),
            topic: topic.to_string(),
            detected_at: at,
            old_hash,
            new_hash,
            sample: sample.join("\n"),
            status: CandidateStatus::New,
            reason: None,
        }
    }
}

/// Очередь — JSONL-файл: одна строка JSON на кандидата. Хвост дописывается,
/// статусы правятся перезаписью файла; можно держать в git.
pub struct CandidateQueue {
    path: std::path::PathBuf,
}

impl CandidateQueue {
    pub fn open(path: impl Into<std::path::PathBuf>) -> QueueResult<CandidateQueue> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if !path.exists() {
            std::fs::write(&path, "")?;
        }
        Ok(CandidateQueue { path })
    }

    /// Путь файла очереди (для обслуживания и тестов).
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn append(&self, candidate: &Candidate) -> QueueResult<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).open(&self.path)?;
        writeln!(file, "{}", serde_json::to_string(candidate)?)?;
        Ok(())
    }

    /// Все кандидаты в порядке файла.
    pub fn all(&self) -> QueueResult<Vec<Candidate>> {
        let text = std::fs::read_to_string(&self.path)?;
        let mut out = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            out.push(serde_json::from_str(line)?);
        }
        Ok(out)
    }

    pub fn by_status(&self, status: CandidateStatus) -> QueueResult<Vec<Candidate>> {
        Ok(self
            .all()?
            .into_iter()
            .filter(|c| c.status == status)
            .collect())
    }

    /// Сменить статус кандидата с указанием причины (для rejected).
    pub fn set_status(
        &self,
        id: &str,
        status: CandidateStatus,
        reason: Option<&str>,
    ) -> QueueResult<bool> {
        let mut all = self.all()?;
        let mut changed = false;
        for candidate in &mut all {
            if candidate.id == id {
                candidate.status = status;
                candidate.reason = reason.map(str::to_string);
                changed = true;
            }
        }
        if changed {
            self.rewrite(&all)?;
        }
        Ok(changed)
    }

    fn rewrite(&self, all: &[Candidate]) -> QueueResult<()> {
        let mut text = String::new();
        for candidate in all {
            text.push_str(&serde_json::to_string(candidate)?);
            text.push('\n');
        }
        std::fs::write(&self.path, text)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_list_accept_reject() {
        let dir = std::env::temp_dir().join(format!("pactum-scan-q-{}", std::process::id()));
        let path = dir.join("candidates.jsonl");
        let queue = CandidateQueue::open(&path).unwrap();
        let c1 = Candidate::new(
            "blog",
            "blog",
            None,
            "aaa".into(),
            "первая строка\nвторая".as_bytes(),
            42,
        );
        let c2 = Candidate::new(
            "disk",
            "disk",
            Some("bbb".into()),
            "ccc".into(),
            b"df output",
            43,
        );
        queue.append(&c1).unwrap();
        queue.append(&c2).unwrap();
        assert_eq!(queue.all().unwrap().len(), 2);
        assert_eq!(queue.by_status(CandidateStatus::New).unwrap().len(), 2);
        assert!(queue
            .set_status(&c1.id, CandidateStatus::Accepted, None)
            .unwrap());
        assert!(queue
            .set_status(
                &c2.id,
                CandidateStatus::Rejected,
                Some("место на диске — не новость")
            )
            .unwrap());
        let all = queue.all().unwrap();
        assert_eq!(all[0].status, CandidateStatus::Accepted);
        assert_eq!(all[1].status, CandidateStatus::Rejected);
        assert_eq!(
            all[1].reason.as_deref(),
            Some("место на диске — не новость")
        );
        // сэмпл: не более 5 строк
        assert_eq!(c1.sample.lines().count(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sample_truncates_to_five_lines() {
        let content = (1..=10)
            .map(|i| format!("строка {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let c = Candidate::new("s", "s", None, "h".into(), content.as_bytes(), 0);
        assert_eq!(c.sample.lines().count(), 5);
        assert!(c.sample.ends_with("строка 5"));
    }
}
