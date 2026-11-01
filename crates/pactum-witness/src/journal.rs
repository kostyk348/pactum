//! Журнал нотариуса: приёмники, связанные хешами, в append-only JSONL.
//!
//! Каждая новая запись содержит хеш предыдущей — тот же принцип, что и в
//! `pactum-core::facts`. Журнал хранится одним файлом `receipts.jsonl`:
//! строка за строкой, без перезаписей. При загрузке цепочка проверяется
//! целиком — повреждённый журнал сервис честно не поднимает.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use pactum_core::hash;
use pactum_identity::KeyPair;
use serde_json::json;

use crate::error::{WitnessError, WitnessResult};
use crate::receipt::{verify_receipt, ChainHead, Receipt};

/// Имя файла журнала внутри рабочей папки.
pub const JOURNAL_FILE: &str = "receipts.jsonl";
/// Папка для сохранённого содержимого (blobs).
pub const BLOBS_DIR: &str = "blobs";

/// Что нужно от нотаризации, кроме самих байтов.
#[derive(Debug, Clone, Default)]
pub struct ReceiptDraft {
    /// SHA-256 содержимого, 64 hex.
    pub content_sha256: String,
    /// Размер в байтах.
    pub size: u64,
    /// Заявленный media type.
    pub media_type: Option<String>,
    /// Заметка (url, имя файла).
    pub note: Option<String>,
    /// Сохранить ли содержимое в blobs/.
    pub stored: bool,
}

/// Журнал приёмников: цепочка + индекс по дайджесту.
#[derive(Debug, Default)]
pub struct Journal {
    receipts: Vec<Receipt>,
    index: HashMap<String, usize>,
    path: PathBuf,
}

impl Journal {
    /// Открыть журнал из файла. Если файла нет — пустой; если есть —
    /// цепочка проверяется целиком, и повреждённый журнал не открывается.
    pub fn open(path: &Path) -> WitnessResult<Journal> {
        let mut journal = Journal {
            receipts: Vec::new(),
            index: HashMap::new(),
            path: path.to_path_buf(),
        };
        if path.exists() {
            let text = std::fs::read_to_string(path)?;
            for (line_no, line) in text.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let receipt: Receipt = serde_json::from_str(line).map_err(|err| {
                    WitnessError::ChainBroken(
                        (line_no + 1) as u64,
                        format!("строка не приёмник: {err}"),
                    )
                })?;
                journal.receipts.push(receipt);
            }
            journal.rebuild_index();
            journal.verify()?;
        }
        Ok(journal)
    }

    /// Открыть журнал без проверки цепочки (только с громким варнингом).
    /// Для восстановления данных, не для работы.
    pub fn open_unchecked(path: &Path) -> WitnessResult<Journal> {
        let mut journal = Journal {
            receipts: Vec::new(),
            index: HashMap::new(),
            path: path.to_path_buf(),
        };
        if path.exists() {
            let text = std::fs::read_to_string(path)?;
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                journal.receipts.push(serde_json::from_str(line)?);
            }
            journal.rebuild_index();
            eprintln!("witness: ВНИМАНИЕ — журнал открыт без проверки целостности");
        }
        Ok(journal)
    }

    fn rebuild_index(&mut self) {
        self.index = self
            .receipts
            .iter()
            .enumerate()
            .map(|(i, r)| (r.content_sha256.clone(), i))
            .collect();
    }

    /// Проверить цепочку: подпись и хеш каждой записи + связность.
    pub fn verify(&self) -> WitnessResult<()> {
        let mut prev = String::new();
        for (i, receipt) in self.receipts.iter().enumerate() {
            if receipt.seq != i as u64 + 1 {
                return Err(WitnessError::ChainBroken(
                    receipt.seq,
                    "нарушена нумерация".into(),
                ));
            }
            if receipt.prev_hash != prev {
                return Err(WitnessError::ChainBroken(
                    receipt.seq,
                    "хеш предыдущей записи не сходится".into(),
                ));
            }
            verify_receipt(receipt).map_err(|err| {
                WitnessError::ChainBroken(receipt.seq, format!("приёмник не подтверждён: {err}"))
            })?;
            prev = receipt.entry_hash.clone();
        }
        Ok(())
    }

    /// Записей в журнале.
    pub fn len(&self) -> usize {
        self.receipts.len()
    }

    /// Пуст ли журнал.
    pub fn is_empty(&self) -> bool {
        self.receipts.is_empty()
    }

    /// Приёмник по дайджесту содержимого.
    pub fn get(&self, digest: &str) -> Option<&Receipt> {
        self.index.get(digest).map(|i| &self.receipts[*i])
    }

    /// Срез журнала (для листинга).
    pub fn slice(&self, offset: usize, limit: usize) -> Vec<&Receipt> {
        self.receipts
            .iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>()
    }

    /// Подписанная голова цепочки, если журнал не пуст.
    pub fn head(&self) -> Option<ChainHead> {
        self.receipts.last().map(|r| ChainHead {
            seq: r.seq,
            head_hash: r.entry_hash.clone(),
            ts: r.ts.clone(),
            sig: String::new(),
        })
    }

    /// Дописать приёмник в цепочку и на диск. Хеши, номер и подпись
    /// вычисляются здесь; вручную их подсовывать нельзя.
    pub fn append(&mut self, draft: ReceiptDraft, key: &KeyPair) -> WitnessResult<Receipt> {
        let mut receipt = Receipt {
            v: crate::receipt::RECEIPT_V,
            witness: String::new(),
            pubkey: String::new(),
            seq: self.receipts.len() as u64 + 1,
            ts: pactum_core::time::now_iso(),
            content_sha256: draft.content_sha256,
            size: draft.size,
            media_type: draft.media_type,
            note: draft.note,
            stored: draft.stored,
            prev_hash: self
                .receipts
                .last()
                .map(|r| r.entry_hash.clone())
                .unwrap_or_default(),
            entry_hash: String::new(),
            sig: String::new(),
        };
        receipt.seal(key);
        debug_assert!(verify_receipt(&receipt).is_ok());

        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{}", serde_json::to_string(&receipt)?)?;
        self.index
            .insert(receipt.content_sha256.clone(), self.receipts.len());
        self.receipts.push(receipt.clone());
        Ok(receipt)
    }

    /// Весь журнал одним NDJSON-текстом (для экспорта и аудита).
    pub fn export_jsonl(&self) -> String {
        let mut out = String::new();
        for receipt in &self.receipts {
            out.push_str(&serde_json::to_string(receipt).unwrap_or_default());
            out.push('\n');
        }
        out
    }

    /// Итоговое резюме для /health и CLI.
    pub fn summary(&self) -> serde_json::Value {
        json!({
            "receipts": self.receipts.len(),
            "head": self.head().map(|h| json!({
                "seq": h.seq,
                "head_hash": h.head_hash,
                "ts": h.ts,
            })),
        })
    }
}

/// Папка blobs: содержимое по имени дайджеста.
pub fn blob_path(dir: &Path, digest: &str) -> PathBuf {
    dir.join(BLOBS_DIR).join(digest)
}

/// Сохранить содержимое в blobs/. Возвращает сохранённый размер.
pub fn save_blob(dir: &Path, digest: &str, bytes: &[u8]) -> WitnessResult<u64> {
    let path = blob_path(dir, digest);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, bytes)?;
    Ok(bytes.len() as u64)
}

/// Прочитать сохранённое содержимое, если оно хранится.
pub fn load_blob(dir: &Path, digest: &str) -> WitnessResult<Option<Vec<u8>>> {
    let path = blob_path(dir, digest);
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(std::fs::read(&path)?))
}

/// Контрольная сверка блоба с его приёмником: содержимое на диске не молча
/// подменили? SHA-256 обязана сойтись.
pub fn check_blob(dir: &Path, receipt: &Receipt) -> WitnessResult<()> {
    if !receipt.stored {
        return Ok(());
    }
    match load_blob(dir, &receipt.content_sha256)? {
        Some(bytes) => {
            let actual = hash::sha256_hex(&bytes);
            if actual != receipt.content_sha256 {
                return Err(WitnessError::Hash(format!(
                    "сохранённое содержимое #{seq} не сходится: на диске {actual}",
                    seq = receipt.seq
                )));
            }
            Ok(())
        }
        None => Err(WitnessError::NotFound(format!(
            "приёмник #{seq} обещает blob, а его нет",
            seq = receipt.seq
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::receipt::RECEIPT_V;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pactum-witness-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn draft(text: &str) -> ReceiptDraft {
        ReceiptDraft {
            content_sha256: hash::sha256_text(text),
            size: text.len() as u64,
            media_type: Some("text/plain".into()),
            note: Some(text.into()),
            stored: false,
        }
    }

    #[test]
    fn append_and_verify() {
        let dir = tmpdir("append");
        let key = KeyPair::generate();
        let mut journal = Journal::open(&dir.join(JOURNAL_FILE)).unwrap();
        journal.append(draft("первый"), &key).unwrap();
        journal.append(draft("второй"), &key).unwrap();
        assert_eq!(journal.len(), 2);
        assert!(journal.verify().is_ok());
        let head = journal.head().unwrap();
        assert_eq!(head.seq, 2);
        assert_eq!(
            head.head_hash,
            journal
                .get(&hash::sha256_text("второй"))
                .unwrap()
                .entry_hash
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn persistence_roundtrip() {
        let dir = tmpdir("persist");
        let key = KeyPair::generate();
        let path = dir.join(JOURNAL_FILE);
        {
            let mut journal = Journal::open(&path).unwrap();
            journal.append(draft("раз"), &key).unwrap();
            journal.append(draft("два"), &key).unwrap();
        }
        let reloaded = Journal::open(&path).unwrap();
        assert_eq!(reloaded.len(), 2);
        assert!(reloaded.verify().is_ok());
        assert!(reloaded.get(&hash::sha256_text("раз")).is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tampered_journal_refuses_to_open() {
        let dir = tmpdir("tamper");
        let key = KeyPair::generate();
        let path = dir.join(JOURNAL_FILE);
        {
            let mut journal = Journal::open(&path).unwrap();
            journal.append(draft("оригинал"), &key).unwrap();
        }
        // задним числом переписываем заметку
        let text = std::fs::read_to_string(&path).unwrap();
        let forged = text.replace("оригинал", "подделка");
        std::fs::write(&path, forged).unwrap();
        let err = Journal::open(&path).unwrap_err();
        assert!(matches!(err, WitnessError::ChainBroken(1, _)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reordering_breaks_chain() {
        let dir = tmpdir("reorder");
        let key = KeyPair::generate();
        let mut journal = Journal::open(&dir.join(JOURNAL_FILE)).unwrap();
        journal.append(draft("a"), &key).unwrap();
        journal.append(draft("b"), &key).unwrap();
        journal.receipts.swap(0, 1);
        assert!(journal.verify().is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn blobs_roundtrip_and_check() {
        let dir = tmpdir("blobs");
        let key = KeyPair::generate();
        let mut journal = Journal::open(&dir.join(JOURNAL_FILE)).unwrap();
        let mut d = draft("содержимое");
        d.stored = true;
        let receipt = journal.append(d, &key).unwrap();
        save_blob(&dir, &receipt.content_sha256, "содержимое".as_bytes()).unwrap();
        assert!(check_blob(&dir, &receipt).is_ok());
        // подмена блоба ловится сверкой хеша
        save_blob(&dir, &receipt.content_sha256, "другое".as_bytes()).unwrap();
        assert!(matches!(
            check_blob(&dir, &receipt),
            Err(WitnessError::Hash(_))
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn version_field_is_set() {
        let dir = tmpdir("version");
        let key = KeyPair::generate();
        let mut journal = Journal::open(&dir.join(JOURNAL_FILE)).unwrap();
        let receipt = journal.append(draft("v"), &key).unwrap();
        assert_eq!(receipt.v, RECEIPT_V);
        std::fs::remove_dir_all(&dir).ok();
    }
}
