//! Доставка подписчикам и сервисный слой хаба.
//!
//! [`Deliverer`] — трейт доставки: реальная реализация ходит по HTTP,
//! тестовая — mocked. [`HubService`] — то, чем пользуются HTTP-хендлеры и
//! CLI: публикация с реальной доставкой, ретраи, подписка с challenge.

use std::sync::Mutex;
use std::time::Duration;

use thiserror::Error;

use crate::sig;
use crate::types::{HubConfig, HubError, HubState, PublishReport, SubState, Subscription};

/// Итог одной попытки доставки.
#[derive(Debug, Clone, PartialEq)]
pub enum DeliveryOutcome {
    /// 2xx — подписчик взял.
    Delivered,
    /// 404 — подписчик честно отказался («не хочу»).
    Refused,
    /// Не доехало: тайм-аут, 5xx, сеть.
    Failed(String),
}

/// Кто и как доставляет. Абстракция нужна для тестов и для будущих
/// транспортов (очереди, SMTP).
pub trait Deliverer: Send + Sync {
    /// POST тела на callback с подписью; вернуть итог.
    fn deliver(
        &self,
        callback: &str,
        topic: &str,
        body: &[u8],
        secret: Option<&str>,
    ) -> DeliveryOutcome;

    /// GET challenge-проверки подписчика: должен вернуть 200 и тот же challenge.
    fn challenge(&self, callback: &str, topic: &str, challenge: &str) -> Result<(), String>;
}

/// Реальная доставка: HTTP POST с `X-Hub-Signature` и GET challenge.
/// Блокирующий ureq вызывается из spawn_blocking.
pub struct HttpDeliverer {
    pub timeout: Duration,
}

impl Default for HttpDeliverer {
    fn default() -> Self {
        HttpDeliverer {
            timeout: Duration::from_secs(10),
        }
    }
}

impl Deliverer for HttpDeliverer {
    fn deliver(
        &self,
        callback: &str,
        topic: &str,
        body: &[u8],
        secret: Option<&str>,
    ) -> DeliveryOutcome {
        let callback = callback.to_string();
        let topic = topic.to_string();
        let body = body.to_vec();
        let secret = secret.map(str::to_string);
        let timeout = self.timeout;
        std::thread::spawn(move || {
            deliver_blocking(&callback, &topic, &body, secret.as_deref(), timeout)
        })
        .join()
        .unwrap_or(DeliveryOutcome::Failed("поток доставки умер".into()))
    }

    fn challenge(&self, callback: &str, topic: &str, challenge: &str) -> Result<(), String> {
        let url = format!(
            "{callback}?hub.challenge={challenge}&hub.topic={}",
            urlencode(topic)
        );
        let expected = challenge.to_string();
        std::thread::spawn(move || {
            let agent = ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(5))
                .timeout_read(Duration::from_secs(10))
                .build();
            match agent.get(&url).call() {
                Ok(resp) => {
                    let body = resp.into_string().unwrap_or_default();
                    if body.trim() == expected {
                        Ok(())
                    } else {
                        Err("challenge не совпал".into())
                    }
                }
                Err(err) => Err(err.to_string()),
            }
        })
        .join()
        .unwrap_or_else(|_| Err("поток challenge умер".into()))
    }
}

fn deliver_blocking(
    callback: &str,
    topic: &str,
    body: &[u8],
    secret: Option<&str>,
    timeout: Duration,
) -> DeliveryOutcome {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(timeout)
        .build();
    let mut request = agent
        .post(callback)
        .set("Content-Type", "application/octet-stream")
        .set("X-Pactum-Topic", topic);
    if let Some(secret) = secret {
        request = request.set("X-Hub-Signature", &sig::hub_signature(secret, body));
    }
    match request.send_bytes(body) {
        Ok(_resp) => DeliveryOutcome::Delivered,
        Err(ureq::Error::Status(code, _)) => {
            if code == 404 {
                DeliveryOutcome::Refused
            } else {
                DeliveryOutcome::Failed(format!("HTTP {code}"))
            }
        }
        Err(err) => DeliveryOutcome::Failed(err.to_string()),
    }
}

fn urlencode(text: &str) -> String {
    let mut out = String::new();
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

/// Ошибки сервисного слоя.
#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("хаб: {0}")]
    Hub(#[from] HubError),
    #[error("состояние хаба заблокировано")]
    Poisoned,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("callback не подтвердил challenge: {0}")]
    Challenge(String),
}

/// Сервис хаба: состояние + конфиг + доставщик, под Mutex.
pub struct HubService {
    pub state: Mutex<HubState>,
    pub config: HubConfig,
    pub deliverer: std::sync::Arc<dyn Deliverer>,
}

impl HubService {
    pub fn new(config: HubConfig, deliverer: std::sync::Arc<dyn Deliverer>) -> HubService {
        HubService {
            state: Mutex::new(HubState::new()),
            config,
            deliverer,
        }
    }

    pub fn from_state(
        state: HubState,
        config: HubConfig,
        deliverer: std::sync::Arc<dyn Deliverer>,
    ) -> HubService {
        HubService {
            state: Mutex::new(state),
            config,
            deliverer,
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HubState>, ServiceError> {
        self.state.lock().map_err(|_| ServiceError::Poisoned)
    }

    /// Подписка с challenge-проверкой. Возвращает id подписки.
    pub fn subscribe(
        &self,
        topic: &str,
        callback: &str,
        secret: Option<&str>,
        lease_seconds: Option<u64>,
    ) -> Result<String, ServiceError> {
        let challenge = pactum_core::id::random_hex(16);
        self.deliverer
            .challenge(callback, topic, &challenge)
            .map_err(ServiceError::Challenge)?;
        let now = pactum_core::time::now();
        let id = self
            .lock()?
            .subscribe(topic, callback, secret, lease_seconds, now)?;
        self.persist()?;
        Ok(id)
    }

    /// Отписка.
    pub fn unsubscribe(&self, topic: &str, callback: &str) -> Result<(), ServiceError> {
        let now = pactum_core::time::now();
        self.lock()?.unsubscribe(topic, callback, now)?;
        self.persist()?;
        Ok(())
    }

    /// Публикация: реальная доставка каждой живой подписке,
    /// честная обработка отказов и постановка в очередь ретраев.
    pub fn publish(&self, topic: &str, body: &[u8]) -> Result<PublishReport, ServiceError> {
        HubState::check_body(body, &self.config)?;
        let now = pactum_core::time::now();
        let mut report = PublishReport::default();
        let mut state = self.lock()?;
        let targets = state.targets_for(topic, now);
        if targets.is_empty() {
            state.record_journal(now, "publish", topic, "живых подписчиков нет");
        }
        for sub in &targets {
            let outcome = self
                .deliverer
                .deliver(&sub.callback, topic, body, sub.secret.as_deref());
            match outcome {
                DeliveryOutcome::Delivered => {
                    report.delivered.push(sub.id.clone());
                    state.record_journal(
                        now,
                        "deliver",
                        topic,
                        &format!("{}: доставлено", sub.callback),
                    );
                }
                DeliveryOutcome::Refused => {
                    report.denied.push(sub.id.clone());
                    if let Some((denied_topic, callback)) = state.mark_denied(&sub.id, now) {
                        // честное уведомление: hub.mode=denied
                        let _ = self.deliverer.deliver(
                            &callback,
                            &denied_topic,
                            format!("hub.mode=denied&hub.topic={}", urlencode(&denied_topic))
                                .as_bytes(),
                            sub.secret.as_deref(),
                        );
                    }
                    state.drop_pending_for(&sub.id);
                }
                DeliveryOutcome::Failed(reason) => {
                    report.queued.push(sub.id.clone());
                    state.record_retry(
                        &sub.id,
                        topic,
                        &crate::types::b64_encode(body),
                        1,
                        &self.config,
                        now,
                    );
                    state.record_journal(
                        now,
                        "retry_queued",
                        topic,
                        &format!("{}: {reason}", sub.callback),
                    );
                }
            }
        }
        drop(state);
        self.persist()?;
        Ok(report)
    }

    /// Один проход по очереди ретраев.
    pub fn retry_pass(&self) -> Result<PublishReport, ServiceError> {
        let now = pactum_core::time::now();
        let mut report = PublishReport::default();
        let mut state = self.lock()?;
        // собрать элементы к доставке до мутаций
        let due: Vec<RetryItem> = state
            .due_pending(now)
            .into_iter()
            .filter_map(|pending| {
                let sub = state
                    .subscriptions
                    .iter()
                    .find(|s| s.id == pending.sub_id)
                    .filter(|s| s.state == SubState::Active)
                    .cloned()?;
                Some(RetryItem {
                    attempts: pending.attempts,
                    sub,
                    body_b64: pending.body_b64.clone(),
                })
            })
            .collect();
        for item in due {
            let body = crate::types::b64_decode(&item.body_b64).unwrap_or_default();
            let outcome = self.deliverer.deliver(
                &item.sub.callback,
                &item.sub.topic,
                &body,
                item.sub.secret.as_deref(),
            );
            match outcome {
                DeliveryOutcome::Delivered => {
                    report.delivered.push(item.sub.id.clone());
                    state.drop_pending_for(&item.sub.id);
                    state.record_journal(
                        now,
                        "deliver",
                        &item.sub.topic,
                        "доставлено после ретрая",
                    );
                }
                DeliveryOutcome::Refused => {
                    report.denied.push(item.sub.id.clone());
                    if let Some((_topic, callback)) = state.mark_denied(&item.sub.id, now) {
                        let _ = self.deliverer.deliver(
                            &callback,
                            &item.sub.topic,
                            b"hub.mode=denied",
                            item.sub.secret.as_deref(),
                        );
                    }
                    state.drop_pending_for(&item.sub.id);
                }
                DeliveryOutcome::Failed(_) => {
                    report.queued.push(item.sub.id.clone());
                    state.drop_pending_for(&item.sub.id);
                    state.record_retry(
                        &item.sub.id,
                        &item.sub.topic,
                        &item.body_b64,
                        item.attempts + 1,
                        &self.config,
                        now,
                    );
                }
            }
        }
        drop(state);
        self.persist()?;
        Ok(report)
    }

    /// Обслуживание: снять истёкшие аренды.
    pub fn expire_leases(&self) -> Result<usize, ServiceError> {
        let now = pactum_core::time::now();
        let count = self.lock()?.expire_leases(now);
        self.persist()?;
        Ok(count)
    }

    /// Снимок состояния для API.
    pub fn snapshot(&self) -> Result<HubState, ServiceError> {
        Ok(self.lock()?.clone())
    }

    fn persist(&self) -> Result<(), ServiceError> {
        if let Some(path) = self.config.store_path.clone() {
            let state = self.lock()?.clone();
            crate::store::save_state(&state, &path)?;
        }
        Ok(())
    }
}

struct RetryItem {
    attempts: u32,
    sub: Subscription,
    body_b64: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockDeliverer {
        calls: AtomicUsize,
        behavior: fn(&str, &[u8]) -> DeliveryOutcome,
    }

    impl Deliverer for MockDeliverer {
        fn deliver(
            &self,
            _callback: &str,
            _topic: &str,
            body: &[u8],
            _secret: Option<&str>,
        ) -> DeliveryOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            (self.behavior)(_callback, body)
        }
        fn challenge(&self, _callback: &str, _topic: &str, _challenge: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn mock(behavior: fn(&str, &[u8]) -> DeliveryOutcome) -> std::sync::Arc<dyn Deliverer> {
        std::sync::Arc::new(MockDeliverer {
            calls: AtomicUsize::new(0),
            behavior,
        })
    }

    fn ok_delivery(_cb: &str, _body: &[u8]) -> DeliveryOutcome {
        DeliveryOutcome::Delivered
    }

    fn refused_delivery(_cb: &str, _body: &[u8]) -> DeliveryOutcome {
        DeliveryOutcome::Refused
    }

    fn failed_delivery(_cb: &str, _body: &[u8]) -> DeliveryOutcome {
        DeliveryOutcome::Failed("соединение сброшено".into())
    }

    #[test]
    fn publish_delivers_to_active_subs() {
        let service = HubService::new(HubConfig::default(), mock(ok_delivery));
        service
            .subscribe("news", "http://a/cb", None, None)
            .unwrap();
        let report = service.publish("news", b"hello").unwrap();
        assert_eq!(report.delivered.len(), 1);
        assert!(report.queued.is_empty());
    }

    #[test]
    fn refusal_404_is_honest_and_notified() {
        let service = HubService::new(HubConfig::default(), mock(refused_delivery));
        service
            .subscribe("news", "http://a/cb", None, None)
            .unwrap();
        let report = service.publish("news", b"hello").unwrap();
        assert_eq!(report.denied.len(), 1);
        let state = service.snapshot().unwrap();
        assert_eq!(state.subscriptions[0].state, SubState::Denied);
        assert!(state.journal.iter().any(|e| e.action == "denied"));
    }

    #[test]
    fn failed_delivery_goes_to_retry_queue() {
        let config = HubConfig {
            retry_first_delay_s: 0,
            retry_max_delay_s: 0,
            retry_max_attempts: 3,
            ..Default::default()
        };
        let service = HubService::new(config, mock(failed_delivery));
        service
            .subscribe("news", "http://a/cb", None, None)
            .unwrap();
        let report = service.publish("news", b"hello").unwrap();
        assert_eq!(report.queued.len(), 1);
        assert_eq!(service.snapshot().unwrap().pending.len(), 1);

        // публикация = попытка 1; ретрай = попытка 2; второй ретрай = попытка 3 → исчерпание
        let r1 = service.retry_pass().unwrap();
        assert_eq!(r1.queued.len(), 1);
        let r2 = service.retry_pass().unwrap();
        assert_eq!(r2.queued.len(), 1);
        let state = service.snapshot().unwrap();
        assert!(state.pending.is_empty());
        assert_eq!(state.subscriptions[0].state, SubState::Degraded);
    }

    #[test]
    fn retry_after_success_delivers() {
        let calls = AtomicUsize::new(0);
        let deliverer: std::sync::Arc<dyn Deliverer> =
            std::sync::Arc::new(FlakyDeliverer { calls });
        let config = HubConfig {
            retry_first_delay_s: 0,
            retry_max_delay_s: 0,
            retry_max_attempts: 8,
            ..Default::default()
        };
        let service = HubService::new(config, deliverer);
        service
            .subscribe("news", "http://a/cb", None, None)
            .unwrap();
        service.publish("news", b"hello").unwrap();
        let r1 = service.retry_pass().unwrap();
        assert_eq!(r1.queued.len(), 1);
        let report = service.retry_pass().unwrap();
        assert_eq!(report.delivered.len(), 1);
        assert!(service.snapshot().unwrap().pending.is_empty());
    }

    struct FlakyDeliverer {
        calls: AtomicUsize,
    }

    impl Deliverer for FlakyDeliverer {
        fn deliver(&self, _cb: &str, _t: &str, _b: &[u8], _s: Option<&str>) -> DeliveryOutcome {
            if self.calls.fetch_add(1, Ordering::SeqCst) < 2 {
                DeliveryOutcome::Failed("флапаем".into())
            } else {
                DeliveryOutcome::Delivered
            }
        }
        fn challenge(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn oversize_publish_is_rejected() {
        let config = HubConfig {
            max_body: 4,
            ..Default::default()
        };
        let service = HubService::new(config, mock(ok_delivery));
        let err = service.publish("news", b"12345").unwrap_err();
        assert!(matches!(
            err,
            ServiceError::Hub(HubError::BodyTooLarge { .. })
        ));
    }
}
