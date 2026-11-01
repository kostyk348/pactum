//! Сервис нотариуса: приём содержимого, журнал, blob-хранилище, checkpoint.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use pactum_core::hash;
use pactum_identity::KeyPair;
use serde_json::json;

use crate::error::WitnessResult;
use crate::fetch::{fetch_url, UrlLimits};
use crate::journal::{save_blob, Journal, ReceiptDraft, JOURNAL_FILE};
use crate::rate::RateLimiter;
use crate::receipt::{verify_receipt, Receipt};

/// Потолок тела по умолчанию: 16 МиБ.
pub const DEFAULT_MAX_BODY: usize = 16 * 1024 * 1024;
/// Лимит запросов по умолчанию: 240 в минуту на IP.
pub const DEFAULT_RATE_PER_MIN: u32 = 240;
/// Топик checkpoint-ов по умолчанию.
pub const DEFAULT_HUB_TOPIC: &str = "witness.head";
/// Период checkpoint-ов по умолчанию: каждая 25-я запись.
pub const DEFAULT_CHECKPOINT_EVERY: u64 = 25;
/// Потолок длины заметки: журнал не место для простыней.
pub const NOTE_MAX_CHARS: usize = 512;

/// Настройки сервиса.
#[derive(Debug, Clone)]
pub struct WitnessConfig {
    /// Максимум байтов в одном notarize.
    pub max_body: usize,
    /// Запросов на IP в минуту.
    pub rate_per_min: u32,
    /// Куда публиковать checkpoint-ы (base url хаба), если вообще.
    pub hub_url: Option<String>,
    /// Топик checkpoint-ов.
    pub hub_topic: String,
    /// Каждая N-я запись публикует подписанную голову в хаб.
    pub checkpoint_every: u64,
    /// Правила чтения URL для /notarize/url.
    pub url_limits: UrlLimits,
}

impl Default for WitnessConfig {
    fn default() -> WitnessConfig {
        WitnessConfig {
            max_body: DEFAULT_MAX_BODY,
            rate_per_min: DEFAULT_RATE_PER_MIN,
            hub_url: None,
            hub_topic: DEFAULT_HUB_TOPIC.to_string(),
            checkpoint_every: DEFAULT_CHECKPOINT_EVERY,
            url_limits: UrlLimits {
                max_bytes: DEFAULT_MAX_BODY,
                ..UrlLimits::default()
            },
        }
    }
}

/// Результат нотаризации: приёмник + признак повтора.
#[derive(Debug, Clone)]
pub struct NotarizeOutcome {
    pub receipt: Receipt,
    /// Документ с таким дайджестом уже был нотариован: приёмник прежний,
    /// журнал не растёт. Честно помечено, а не спрятано.
    pub duplicate: bool,
}

/// Состояние нотариуса: ключ, журнал, лимиты, blob-папка.
pub struct WitnessService {
    key: KeyPair,
    dir: PathBuf,
    cfg: WitnessConfig,
    journal: Mutex<Journal>,
    limiter: Mutex<RateLimiter>,
}

impl WitnessService {
    /// Открыть сервис в папке `dir`: журнал проверяется при подъёме,
    /// повреждённая история сервис не обслуживает.
    pub fn open(dir: &Path, key: KeyPair, cfg: WitnessConfig) -> WitnessResult<WitnessService> {
        std::fs::create_dir_all(dir)?;
        let journal = Journal::open(&dir.join(JOURNAL_FILE))?;
        journal.verify()?;
        let limiter = RateLimiter::new(cfg.rate_per_min);
        Ok(WitnessService {
            key,
            dir: dir.to_path_buf(),
            cfg,
            journal: Mutex::new(journal),
            limiter: Mutex::new(limiter),
        })
    }

    pub fn node_id(&self) -> String {
        self.key.node_id()
    }

    pub fn pubkey_hex(&self) -> String {
        self.key.pubkey_hex()
    }

    pub fn config(&self) -> &WitnessConfig {
        &self.cfg
    }

    /// Проверить лимит частоты для клиента. `false` — отказ `429`.
    pub fn check_rate(&self, ip: IpAddr) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.limiter
            .lock()
            .map(|mut l| l.check(ip, now))
            .unwrap_or(false)
    }

    /// Нотаризовать байты. Идемпотентно: тот же дайджест — тот же приёмник.
    pub fn notarize(
        &self,
        bytes: &[u8],
        media_type: Option<String>,
        note: Option<String>,
        store: bool,
    ) -> WitnessResult<NotarizeOutcome> {
        let digest = hash::sha256_hex(bytes);
        {
            let journal = self.journal.lock().map_err(|_| poisoned())?;
            if let Some(receipt) = journal.get(&digest) {
                return Ok(NotarizeOutcome {
                    receipt: receipt.clone(),
                    duplicate: true,
                });
            }
        }
        if store {
            save_blob(&self.dir, &digest, bytes)?;
        }
        let draft = ReceiptDraft {
            content_sha256: digest,
            size: bytes.len() as u64,
            media_type,
            note,
            stored: store,
        };
        let outcome = {
            let mut journal = self.journal.lock().map_err(|_| poisoned())?;
            let receipt = journal.append(draft, &self.key)?;
            NotarizeOutcome {
                receipt,
                duplicate: false,
            }
        };
        self.maybe_checkpoint(outcome.receipt.seq);
        Ok(outcome)
    }

    /// Нотаризовать URL: скачать (с лимитами и проверкой адресов),
    /// посчитать хеш и записать приёмник. Заметка — итоговый url.
    pub fn notarize_url(&self, url: &str, store: bool) -> WitnessResult<NotarizeOutcome> {
        let fetched = fetch_url(url, &self.cfg.url_limits)?;
        let content_type = fetched.content_type.clone();
        let note = fetched.url.clone();
        self.notarize(&fetched.body, content_type, Some(note), store)
    }

    /// Каждая N-я запись — checkpoint в хаб. Не прошёл — журнал громко
    /// ругается в stderr и продолжает работу: ничего не теряется молча.
    fn maybe_checkpoint(&self, seq: u64) {
        if self.cfg.hub_url.is_none() || self.cfg.checkpoint_every == 0 {
            return;
        }
        if seq % self.cfg.checkpoint_every != 0 {
            return;
        }
        let result = self.publish_checkpoint_now();
        match result {
            Ok(()) => eprintln!("witness: checkpoint #{seq} опубликован в хаб"),
            Err(err) => eprintln!("witness: checkpoint #{seq} НЕ ушёл в хаб: {err}"),
        }
    }

    /// Опубликовать подписанную голову журнала в хаб (сейчас, синхронно).
    pub fn publish_checkpoint_now(&self) -> WitnessResult<()> {
        let hub_url = self
            .cfg
            .hub_url
            .clone()
            .ok_or_else(|| crate::error::WitnessError::BadUrl("хаб не настроен".into()))?;
        let head = self.signed_head()?.ok_or_else(|| {
            crate::error::WitnessError::NotFound("журнал пуст — головы нет".into())
        })?;
        let topic = self.cfg.hub_topic.clone();
        let url = format!(
            "{}/hub/publish?topic={}",
            hub_url.trim_end_matches('/'),
            urlencode(&topic)
        );
        let body = serde_json::to_string(&json!({
            "kind": "witness.checkpoint",
            "head": head,
            "pubkey": self.key.pubkey_hex(),
            "witness": self.key.node_id(),
        }))?;
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(5))
            .timeout_read(std::time::Duration::from_secs(10))
            .build();
        let response = agent
            .post(&url)
            .set("Content-Type", "application/json")
            .send_string(&body)
            .map_err(|err| crate::error::WitnessError::Network(err.to_string()))?;
        eprintln!(
            "witness: хаб ответил {} на checkpoint seq={}",
            response.status(),
            head.seq
        );
        Ok(())
    }

    /// Подписанная голова журнала (checkpoint).
    pub fn signed_head(&self) -> WitnessResult<Option<crate::receipt::ChainHead>> {
        let journal = self.journal.lock().map_err(|_| poisoned())?;
        Ok(journal.head().map(|mut h| {
            h.sign(&self.key);
            h
        }))
    }

    /// Приёмник по дайджесту + живая перепроверка подписи.
    pub fn verified_receipt(&self, digest: &str) -> WitnessResult<Receipt> {
        let receipt = self
            .journal
            .lock()
            .map_err(|_| poisoned())?
            .get(digest)
            .cloned()
            .ok_or_else(|| crate::error::WitnessError::NotFound(digest.to_string()))?;
        verify_receipt(&receipt)?;
        Ok(receipt)
    }

    /// Листинг журнала (offset, limit) + всего записей.
    pub fn list(&self, offset: usize, limit: usize) -> WitnessResult<(usize, Vec<Receipt>)> {
        let journal = self.journal.lock().map_err(|_| poisoned())?;
        let total = journal.len();
        let receipts = journal.slice(offset, limit).into_iter().cloned().collect();
        Ok((total, receipts))
    }

    /// Сохранённое содержимое (если хранили), с контрольной сверкой хеша.
    pub fn blob(&self, digest: &str) -> WitnessResult<Option<Vec<u8>>> {
        let receipt = self.verified_receipt(digest)?;
        if !receipt.stored {
            return Ok(None);
        }
        let bytes = crate::journal::load_blob(&self.dir, digest)?
            .ok_or_else(|| crate::error::WitnessError::NotFound(format!("blob {digest}")))?;
        let actual = hash::sha256_hex(&bytes);
        if actual != digest {
            return Err(crate::error::WitnessError::Hash(format!(
                "сохранённое содержимое не сходится: на диске {actual}"
            )));
        }
        Ok(Some(bytes))
    }

    /// Весь журнал NDJSON (для экспорта и аудита).
    pub fn export_jsonl(&self) -> WitnessResult<String> {
        Ok(self.journal.lock().map_err(|_| poisoned())?.export_jsonl())
    }

    /// Резюме для /health.
    pub fn summary(&self) -> WitnessResult<serde_json::Value> {
        Ok(self.journal.lock().map_err(|_| poisoned())?.summary())
    }
}

fn poisoned() -> crate::error::WitnessError {
    crate::error::WitnessError::Network("внутренняя блокировка отравлена".into())
}

/// Процентное кодирование для query-параметров (минимальное).
fn urlencode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::WitnessError;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pactum-wsvc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn service(tag: &str) -> WitnessService {
        WitnessService::open(&tmpdir(tag), KeyPair::generate(), WitnessConfig::default()).unwrap()
    }

    #[test]
    fn notarize_and_duplicate() {
        let svc = service("dup");
        let first = svc
            .notarize("отчёт за квартал".as_bytes(), None, None, false)
            .unwrap();
        assert!(!first.duplicate);
        assert_eq!(first.receipt.seq, 1);
        let second = svc
            .notarize("отчёт за квартал".as_bytes(), None, None, false)
            .unwrap();
        assert!(second.duplicate);
        assert_eq!(second.receipt.seq, 1, "приёмник прежний — журнал не растёт");
        let third = svc
            .notarize("другой документ".as_bytes(), None, None, false)
            .unwrap();
        assert_eq!(third.receipt.seq, 2);
    }

    #[test]
    fn stored_blob_roundtrip() {
        let svc = service("blob");
        let out = svc
            .notarize(
                "сохранить меня".as_bytes(),
                Some("text/plain".into()),
                None,
                true,
            )
            .unwrap();
        let bytes = svc.blob(&out.receipt.content_sha256).unwrap().unwrap();
        assert_eq!(bytes, "сохранить меня".as_bytes());
        // без store=1 — честный ответ «не хранилось»
        let out = svc
            .notarize("не сохранять".as_bytes(), None, None, false)
            .unwrap();
        assert!(svc.blob(&out.receipt.content_sha256).unwrap().is_none());
    }

    #[test]
    fn chain_verify_and_head_signature() {
        let svc = service("head");
        for i in 0..3 {
            svc.notarize(format!("документ {i}").as_bytes(), None, None, false)
                .unwrap();
        }
        let head = svc.signed_head().unwrap().unwrap();
        assert_eq!(head.seq, 3);
        assert!(head.verify(&svc.pubkey_hex()).is_ok());
        let receipt = svc
            .verified_receipt(&hash::sha256_text("документ 1"))
            .unwrap();
        assert_eq!(receipt.seq, 2);
    }

    #[test]
    fn rate_limiter_gates_service() {
        let dir = tmpdir("rate");
        let cfg = WitnessConfig {
            rate_per_min: 2,
            ..WitnessConfig::default()
        };
        let svc = WitnessService::open(&dir, KeyPair::generate(), cfg).unwrap();
        let ip = "203.0.113.7".parse().unwrap();
        assert!(svc.check_rate(ip));
        assert!(svc.check_rate(ip));
        assert!(!svc.check_rate(ip));
    }

    #[test]
    fn export_then_reload_elsewhere() {
        let svc = service("export");
        svc.notarize(b"a", None, None, false).unwrap();
        svc.notarize(b"b", None, None, false).unwrap();
        let jsonl = svc.export_jsonl().unwrap();
        let dir = tmpdir("export-reload");
        let path = dir.join(JOURNAL_FILE);
        std::fs::write(&path, &jsonl).unwrap();
        let journal = Journal::open(&path).unwrap();
        assert_eq!(journal.len(), 2);
        assert!(journal.verify().is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn url_notarize_against_local_origin() {
        // локальный источник: разрешаем приватные адреса явно
        let dir = tmpdir("url");
        let cfg = WitnessConfig {
            url_limits: UrlLimits {
                allow_private: true,
                max_bytes: 1 << 20,
                ..UrlLimits::default()
            },
            ..WitnessConfig::default()
        };
        let svc = WitnessService::open(&dir, KeyPair::generate(), cfg).unwrap();

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let (addr, _task) = rt.block_on(async {
            let app = axum::Router::new().route(
                "/doc",
                axum::routing::get(|| async { "страница для свидетелей" }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            (addr, task)
        });

        let out = svc
            .notarize_url(&format!("http://{addr}/doc"), false)
            .unwrap();
        assert_eq!(
            out.receipt.content_sha256,
            hash::sha256_text("страница для свидетелей")
        );
        assert!(out.receipt.note.unwrap().ends_with("/doc"));
        assert!(
            out.receipt
                .media_type
                .as_deref()
                .unwrap_or_default()
                .starts_with("text/plain"),
            "media_type: {:?}",
            out.receipt.media_type
        );
    }

    #[test]
    fn url_notarize_refuses_private_by_default() {
        let svc = service("ssrf");
        let err = svc.notarize_url("http://127.0.0.1:9/x", false).unwrap_err();
        assert!(matches!(err, WitnessError::BadUrl(_)));
    }
}
