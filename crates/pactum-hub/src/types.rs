//! Типы и чистая логика хаба — без HTTP и без клока.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Максимальная аренда подписки: 24 часа, как в hubd.
pub const MAX_LEASE_S: u64 = 86_400;

/// Ошибки хаба — все честные и говорящие.
#[derive(Debug, Error, PartialEq, Clone)]
pub enum HubError {
    #[error("тело сообщения {actual} байт больше лимита {limit} — молча терять не будем")]
    BodyTooLarge { actual: usize, limit: usize },
    #[error("не задан топик")]
    NoTopic,
    #[error("не задан callback")]
    NoCallback,
    #[error("callback не http(s): {0}")]
    BadCallback(String),
    #[error("подписка {0} не найдена")]
    UnknownSubscription(String),
    #[error("callback не подтвердил challenge")]
    ChallengeFailed,
}

pub type HubResult<T> = Result<T, HubError>;

/// Состояние подписки. Честные три: живая, изношенная, отозванная владельцем.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubState {
    Active,
    /// Доставки падали до исчерпания ретраев — в публикации будет отказ 503.
    Degraded,
    /// Подписчик сам отказался (404 на доставку).
    Denied,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub topic: String,
    pub callback: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    /// До какой секунды аренда жива.
    pub lease_until: u64,
    pub state: SubState,
    #[serde(default)]
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingDelivery {
    pub sub_id: String,
    pub topic: String,
    /// Тело в base64 — файл состояния остаётся читаемым.
    pub body_b64: String,
    pub attempts: u32,
    /// Секунда (от эпохи), когда можно пробовать снова.
    pub next_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub ts: u64,
    pub action: String,
    pub topic: String,
    pub detail: String,
}

/// Конфигурация хаба. Значения по умолчанию — из контракта доставки hubd:
/// первый ретрай через 5 с, удвоение до 1 часа, 8 попыток, тело ≤ 256 КБ.
#[derive(Debug, Clone)]
pub struct HubConfig {
    pub max_body: usize,
    pub max_lease_s: u64,
    pub retry_first_delay_s: u64,
    pub retry_max_delay_s: u64,
    pub retry_max_attempts: u32,
    /// Файл персистентности; None — состояние только в памяти.
    pub store_path: Option<PathBuf>,
}

impl Default for HubConfig {
    fn default() -> Self {
        HubConfig {
            max_body: 256 * 1024,
            max_lease_s: MAX_LEASE_S,
            retry_first_delay_s: 5,
            retry_max_delay_s: 3600,
            retry_max_attempts: 8,
            store_path: None,
        }
    }
}

/// Отчёт о публикации — что с ней стало, по каждому подписчику.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PublishReport {
    pub delivered: Vec<String>,
    pub queued: Vec<String>,
    /// Подписчики, которые отказались (404): сняты с уведомлением denied.
    pub denied: Vec<String>,
}

/// Состояние хаба: подписки + очередь ретраев + журнал.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HubState {
    pub subscriptions: Vec<Subscription>,
    pub pending: Vec<PendingDelivery>,
    #[serde(default)]
    pub journal: Vec<JournalEntry>,
}

impl HubState {
    pub fn new() -> HubState {
        HubState::default()
    }

    /// Запись в журнал (публично: сервис пишет свои события).
    pub fn record_journal(&mut self, ts: u64, action: &str, topic: &str, detail: &str) {
        self.journal(ts, action, topic, detail);
    }

    fn journal(&mut self, ts: u64, action: &str, topic: &str, detail: &str) {
        self.journal.push(JournalEntry {
            ts,
            action: action.to_string(),
            topic: topic.to_string(),
            detail: detail.to_string(),
        });
        let overflow = self.journal.len().saturating_sub(200);
        if overflow > 0 {
            self.journal.drain(0..overflow);
        }
    }

    /// Подписка: создаёт или обновляет (тот же topic+callback — та же подписка,
    /// аренда продлевается, состояние сбрасывается в Active).
    #[allow(clippy::too_many_arguments)]
    pub fn subscribe(
        &mut self,
        topic: &str,
        callback: &str,
        secret: Option<&str>,
        lease_seconds: Option<u64>,
        now: u64,
    ) -> HubResult<String> {
        if topic.trim().is_empty() {
            return Err(HubError::NoTopic);
        }
        if callback.trim().is_empty() {
            return Err(HubError::NoCallback);
        }
        if !(callback.starts_with("http://") || callback.starts_with("https://")) {
            return Err(HubError::BadCallback(callback.to_string()));
        }
        let lease = lease_seconds
            .unwrap_or(HubConfig::default().max_lease_s)
            .min(MAX_LEASE_S);
        let lease_until = now + lease;
        let existing = self
            .subscriptions
            .iter_mut()
            .find(|s| s.topic == topic && s.callback == callback);
        if let Some(existing) = existing {
            existing.secret = secret.map(str::to_string);
            existing.lease_until = lease_until;
            existing.state = SubState::Active;
            existing.created_at = now;
            let id = existing.id.clone();
            self.journal(now, "subscribe", topic, "аренда продлена");
            return Ok(id);
        }
        let id = pactum_core::id::new_object_id("sub");
        self.subscriptions.push(Subscription {
            id: id.clone(),
            topic: topic.to_string(),
            callback: callback.to_string(),
            secret: secret.map(str::to_string),
            lease_until,
            state: SubState::Active,
            created_at: now,
        });
        self.journal(now, "subscribe", topic, callback);
        Ok(id)
    }

    /// Отписка.
    pub fn unsubscribe(&mut self, topic: &str, callback: &str, now: u64) -> HubResult<()> {
        let before = self.subscriptions.len();
        self.subscriptions
            .retain(|s| !(s.topic == topic && s.callback == callback));
        if self.subscriptions.len() == before {
            return Err(HubError::UnknownSubscription(format!(
                "{topic} → {callback}"
            )));
        }
        self.journal(now, "unsubscribe", topic, callback);
        Ok(())
    }

    /// Подписчики топика: живые, с неистёкшей арендой.
    pub fn active_subs(&self, topic: &str, now: u64) -> Vec<&Subscription> {
        self.subscriptions
            .iter()
            .filter(|s| s.topic == topic)
            .filter(|s| s.state == SubState::Active && s.lease_until > now)
            .collect()
    }

    /// Поставить в очередь ретрай с экспоненциальной задержкой.
    fn enqueue_retry(
        &mut self,
        sub_id: &str,
        topic: &str,
        body_b64: &str,
        attempts: u32,
        config: &HubConfig,
        now: u64,
    ) {
        let factor = 1u64 << attempts.min(31);
        let delay =
            (config.retry_first_delay_s.saturating_mul(factor)).min(config.retry_max_delay_s);
        self.pending.push(PendingDelivery {
            sub_id: sub_id.to_string(),
            topic: topic.to_string(),
            body_b64: body_b64.to_string(),
            attempts,
            next_at: now + delay,
        });
    }

    /// Отметить подписку деградировавшей: исчерпаны ретраи.
    pub fn mark_degraded(&mut self, sub_id: &str, now: u64) {
        let info = self
            .subscriptions
            .iter_mut()
            .find(|s| s.id == sub_id)
            .map(|sub| {
                sub.state = SubState::Degraded;
                (sub.topic.clone(), sub.callback.clone())
            });
        if let Some((topic, callback)) = info {
            self.journal(
                now,
                "degraded",
                &topic,
                &format!("{callback}: доставки не проходят, подписка изношена"),
            );
        }
    }

    /// Отметить отказ подписчика (404 = «не хочу») и уведомить denied.
    pub fn mark_denied(&mut self, sub_id: &str, now: u64) -> Option<(String, String)> {
        let sub = self.subscriptions.iter_mut().find(|s| s.id == sub_id)?;
        sub.state = SubState::Denied;
        let topic = sub.topic.clone();
        let callback = sub.callback.clone();
        self.journal(
            now,
            "denied",
            &topic,
            &format!("{callback}: подписчик ответил 404"),
        );
        Some((topic, callback))
    }

    /// Снять подписки с истёкшей арендой. Возвращает количество.
    pub fn expire_leases(&mut self, now: u64) -> usize {
        let expired: Vec<Subscription> = self
            .subscriptions
            .iter()
            .filter(|s| s.lease_until <= now && s.state != SubState::Denied)
            .cloned()
            .collect();
        let count = expired.len();
        for sub in &expired {
            self.journal(now, "lease_expired", &sub.topic, &sub.callback);
        }
        self.subscriptions
            .retain(|s| !(s.lease_until <= now && s.state != SubState::Denied));
        count
    }

    /// Убрать очередь ретраев умерших подписок.
    pub fn drop_pending_for(&mut self, sub_id: &str) {
        self.pending.retain(|p| p.sub_id != sub_id);
    }

    /// Проверить лимит тела. Лимит — с честным отказом, не с потерей.
    pub fn check_body(body: &[u8], config: &HubConfig) -> HubResult<()> {
        if body.len() > config.max_body {
            return Err(HubError::BodyTooLarge {
                actual: body.len(),
                limit: config.max_body,
            });
        }
        Ok(())
    }

    /// Цели публикации в топик: живые подписки с неистёкшей арендой.
    pub fn targets_for(&self, topic: &str, now: u64) -> Vec<Subscription> {
        self.subscriptions
            .iter()
            .filter(|s| s.topic == topic && s.state == SubState::Active && s.lease_until > now)
            .cloned()
            .collect()
    }

    /// Очередь ретраев: что пора доставлять.
    pub fn due_pending(&self, now: u64) -> Vec<&PendingDelivery> {
        self.pending.iter().filter(|p| p.next_at <= now).collect()
    }

    pub fn record_retry(
        &mut self,
        sub_id: &str,
        topic: &str,
        body_b64: &str,
        attempts: u32,
        config: &HubConfig,
        now: u64,
    ) {
        if attempts >= config.retry_max_attempts {
            self.mark_degraded(sub_id, now);
            self.drop_pending_for(sub_id);
        } else {
            self.enqueue_retry(sub_id, topic, body_b64, attempts, config, now);
        }
    }
}

// Компактный base64 (стандартный алфавит, с паддингом) — чтобы не тянуть
// крейт ради одного поля в файле состояния.
pub fn b64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

pub fn b64_decode(text: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some(u32::from(c - b'A')),
            b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let text = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    for chunk in text.as_bytes().chunks(4) {
        let mut n: u32 = 0;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_validates_and_caps_lease() {
        let mut state = HubState::new();
        let id = state
            .subscribe(
                "news.rust",
                "http://127.0.0.1:1/cb",
                Some("secret"),
                Some(MAX_LEASE_S * 10),
                100,
            )
            .unwrap();
        assert!(id.starts_with("sub-"));
        let sub = &state.subscriptions[0];
        // аренда ограничена максимумом, а не запрошенными 10 сутками
        assert_eq!(sub.lease_until, 100 + MAX_LEASE_S);
        assert_eq!(sub.state, SubState::Active);
    }

    #[test]
    fn subscribe_rejects_bad_input() {
        let mut state = HubState::new();
        assert_eq!(
            state.subscribe("", "http://x/cb", None, None, 0),
            Err(HubError::NoTopic)
        );
        assert_eq!(
            state.subscribe("t", "", None, None, 0),
            Err(HubError::NoCallback)
        );
        assert!(matches!(
            state.subscribe("t", "ftp://x", None, None, 0),
            Err(HubError::BadCallback(_))
        ));
    }

    #[test]
    fn resubscribe_renews() {
        let mut state = HubState::new();
        let id1 = state
            .subscribe("t", "http://a/cb", None, Some(100), 0)
            .unwrap();
        let id2 = state
            .subscribe("t", "http://a/cb", None, Some(200), 50)
            .unwrap();
        assert_eq!(id1, id2);
        assert_eq!(state.subscriptions.len(), 1);
        assert_eq!(state.subscriptions[0].lease_until, 250);
    }

    #[test]
    fn publish_size_limit_is_honest() {
        let config = HubConfig {
            max_body: 10,
            ..Default::default()
        };
        let err = HubState::check_body(&[0u8; 11], &config).unwrap_err();
        assert!(matches!(
            err,
            HubError::BodyTooLarge {
                actual: 11,
                limit: 10
            }
        ));
        assert!(HubState::check_body(&[0u8; 10], &config).is_ok());
    }

    #[test]
    fn base64_roundtrip() {
        for data in [
            &b""[..],
            b"a",
            b"ab",
            b"abc",
            b"abcd",
            &[0xff, 0x00, 0x7f][..],
        ] {
            let enc = b64_encode(data);
            assert_eq!(b64_decode(&enc).unwrap(), data);
        }
        // известный вектор
        assert_eq!(b64_encode(b"hello"), "aGVsbG8=");
        assert_eq!(
            b64_encode(b"any carnal pleasure."),
            "YW55IGNhcm5hbCBwbGVhc3VyZS4="
        );
    }

    #[test]
    fn denied_and_degraded_states() {
        let mut state = HubState::new();
        let id = state.subscribe("t", "http://a/cb", None, None, 0).unwrap();
        let (topic, cb) = state.mark_denied(&id, 10).unwrap();
        assert_eq!(topic, "t");
        assert_eq!(cb, "http://a/cb");
        assert_eq!(state.subscriptions[0].state, SubState::Denied);
        state.mark_degraded(&id, 11);
        assert_eq!(state.subscriptions[0].state, SubState::Degraded);
    }

    #[test]
    fn lease_expires() {
        let mut state = HubState::new();
        state
            .subscribe("t", "http://a/cb", None, Some(100), 0)
            .unwrap();
        assert_eq!(state.active_subs("t", 50).len(), 1);
        assert_eq!(state.active_subs("t", 150).len(), 0);
        assert_eq!(state.expire_leases(150), 1);
        assert!(state.subscriptions.is_empty());
    }

    #[test]
    fn journal_is_bounded() {
        let mut state = HubState::new();
        for i in 0..500 {
            state.journal(i, "act", "t", "d");
        }
        assert!(state.journal.len() <= 200);
    }
}
