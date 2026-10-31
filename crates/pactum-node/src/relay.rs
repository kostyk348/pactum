//! Capability-relay: маршрутизатор задач по подпискам на умения.
//!
//! Порт `transport/relay.py` из acp-mvp. Публичный узел-релей: исполнители
//! подписываются на capability, заказчики публикуют письма с заголовком
//! `X-Pactum-Capability`, релей рассылает копии всем подходящим подписчикам.
//! Подписчиков нет — письмо остаётся в очереди релея и ждёт подписки.
//!
//! Capability — произвольный dotted-путь (`ci.compile.rust`), wildcard `*`.
//! Кто подписывается — не важно: сервис, человек, агент.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

/// Запись реестра: кто и по какой подписке ждёт письма.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RelaySubscription {
    pub node_id: String,
    /// Capability с возможным wildcard: `ci.compile.*` или `*`.
    pub capability: String,
    /// Полный URL входной точки узла (его `/inbox`).
    pub endpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RelayState {
    pub subscriptions: Vec<RelaySubscription>,
    /// Письма, ждущие подписчика: (capability, тело письма).
    pub queue: Vec<QueuedLetter>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedLetter {
    pub capability: String,
    /// Тело письма (конверт целиком, текст).
    pub letter: String,
    pub queued_at: u64,
}

#[derive(Debug, Error, PartialEq)]
pub enum RelayError {
    #[error("нет заголовка X-Pactum-Capability")]
    NoCapability,
    #[error("подписка неполная: нужны node_id, capability и endpoint")]
    Incomplete,
    #[error("endpoint не http(s): {0}")]
    BadEndpoint(String),
}

/// Реестр релея. Доставка — HTTP POST письма на endpoint подписчика.
pub struct RelayRegistry {
    pub state: Mutex<RelayState>,
}

impl RelayRegistry {
    pub fn new() -> RelayRegistry {
        RelayRegistry {
            state: Mutex::new(RelayState::default()),
        }
    }

    pub fn from_state(state: RelayState) -> RelayRegistry {
        RelayRegistry {
            state: Mutex::new(state),
        }
    }

    pub fn subscribe(&self, sub: RelaySubscription) -> Result<(), RelayError> {
        if sub.node_id.is_empty() || sub.capability.is_empty() || sub.endpoint.is_empty() {
            return Err(RelayError::Incomplete);
        }
        if !(sub.endpoint.starts_with("http://") || sub.endpoint.starts_with("https://")) {
            return Err(RelayError::BadEndpoint(sub.endpoint));
        }
        let mut state = self.state.lock().map_err(|_| RelayError::Incomplete)?;
        state
            .subscriptions
            .retain(|s| !(s.node_id == sub.node_id && s.capability == sub.capability));
        state.subscriptions.push(sub);
        Ok(())
    }

    /// Кому доставить письмо с этой capability.
    pub fn targets(&self, capability: &str) -> Vec<RelaySubscription> {
        self.state
            .lock()
            .map(|state| {
                state
                    .subscriptions
                    .iter()
                    .filter(|sub| match_matches(&sub.capability, capability))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Поставить письмо в очередь (подписчиков нет).
    pub fn hold(&self, capability: &str, letter: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.queue.push(QueuedLetter {
                capability: capability.to_string(),
                letter: letter.to_string(),
                queued_at: pactum_core::time::now(),
            });
        }
    }

    /// Одна попытка раздачи очереди. Возвращает количество розданных писем.
    pub fn flush_queue(&self) -> usize {
        let mut delivered = 0;
        let mut remaining: Vec<QueuedLetter> = Vec::new();
        if let Ok(mut state) = self.state.lock() {
            let queue = std::mem::take(&mut state.queue);
            for item in queue {
                let targets = state
                    .subscriptions
                    .iter()
                    .filter(|sub| match_matches(&sub.capability, &item.capability))
                    .cloned()
                    .collect::<Vec<_>>();
                if targets.is_empty() {
                    remaining.push(item);
                    continue;
                }
                let mut all_ok = true;
                for target in &targets {
                    if post_letter(&target.endpoint, &item.letter).is_err() {
                        all_ok = false;
                    }
                }
                if all_ok {
                    delivered += 1;
                } else {
                    remaining.push(item);
                }
            }
            state.queue = remaining;
        }
        delivered
    }

    pub fn snapshot(&self) -> RelayState {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

impl Default for RelayRegistry {
    fn default() -> Self {
        RelayRegistry::new()
    }
}

/// Сопоставление подписки и capability: точное, `*`, префикс `prefix.*`.
pub fn match_matches(pattern: &str, capability: &str) -> bool {
    pattern == "*"
        || pattern == capability
        || (pattern.ends_with('*') && capability.starts_with(pattern.trim_end_matches('*')))
}

/// HTTP POST письма на endpoint узла.
fn post_letter(endpoint: &str, letter: &str) -> Result<(), String> {
    let endpoint = endpoint.to_string();
    let letter = letter.to_string();
    std::thread::spawn(move || {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(5))
            .timeout_read(std::time::Duration::from_secs(10))
            .build();
        let result = agent
            .post(&endpoint)
            .set("Content-Type", "text/plain; charset=utf-8")
            .send_string(&letter)
            .map(|_| ())
            .map_err(|err| err.to_string());
        if let Err(ref err) = result {
            eprintln!("relay: доставка на {endpoint} не прошла: {err}");
        }
        result
    })
    .join()
    .unwrap_or_else(|_| Err("поток доставки релея умер".into()))
}

pub type SharedRelay = Arc<RelayRegistry>;

/// Роутер релея: `/relay/*`.
pub fn relay_router(registry: SharedRelay) -> Router {
    Router::new()
        .route("/relay/subscribe", post(relay_subscribe))
        .route("/relay/publish", post(relay_publish))
        .route("/relay/flush", post(relay_flush))
        .route("/relay/subscriptions", get(relay_subscriptions))
        .route("/relay/healthz", get(relay_healthz))
        .with_state(registry)
}

async fn relay_subscribe(
    State(registry): State<SharedRelay>,
    Json(sub): Json<RelaySubscription>,
) -> axum::response::Response {
    match registry.subscribe(sub) {
        Ok(()) => (StatusCode::OK, Json(json!({"ok": true}))).into_response(),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

async fn relay_publish(
    State(registry): State<SharedRelay>,
    headers: HeaderMap,
    letter: Bytes,
) -> axum::response::Response {
    let Some(capability) = headers
        .get("X-Pactum-Capability")
        .and_then(|v| v.to_str().ok())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": RelayError::NoCapability.to_string()})),
        )
            .into_response();
    };
    let text = String::from_utf8_lossy(&letter).to_string();
    let targets = registry.targets(capability);
    if targets.is_empty() {
        registry.hold(capability, &text);
        return (
            StatusCode::ACCEPTED,
            Json(json!({"queued": true, "note": "подписчиков нет — письмо ждёт подписки"})),
        )
            .into_response();
    }
    // блокирующая доставка — только в spawn_blocking: асинхронный исполнитель
    // не должен простаивать, пока письма ходят по сети
    let target_count = targets.len();
    let delivered = tokio::task::spawn_blocking(move || {
        let mut delivered = 0;
        for target in &targets {
            if post_letter(&target.endpoint, &text).is_ok() {
                delivered += 1;
            }
        }
        delivered
    })
    .await
    .unwrap_or(0);
    (
        StatusCode::OK,
        Json(json!({"delivered": delivered, "targets": target_count})),
    )
        .into_response()
}

async fn relay_flush(State(registry): State<SharedRelay>) -> axum::response::Response {
    let delivered = tokio::task::spawn_blocking(move || registry.flush_queue())
        .await
        .unwrap_or(0);
    (StatusCode::OK, Json(json!({"flushed": delivered}))).into_response()
}

async fn relay_subscriptions(State(registry): State<SharedRelay>) -> axum::response::Response {
    let snapshot = registry.snapshot();
    (
        StatusCode::OK,
        Json(json!({"subscriptions": snapshot.subscriptions, "queued": snapshot.queue.len()})),
    )
        .into_response()
}

async fn relay_healthz(State(registry): State<SharedRelay>) -> axum::response::Response {
    let snapshot = registry.snapshot();
    (
        StatusCode::OK,
        Json(json!({"ok": true, "subscriptions": snapshot.subscriptions.len(), "queued": snapshot.queue.len()})),
    )
        .into_response()
}

// Реестр для BTreeMap-ключей (на будущее: статистика по capability).
#[allow(dead_code)]
type CapabilityStats = BTreeMap<String, u64>;
