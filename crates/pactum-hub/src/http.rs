//! HTTP-слой хаба на axum: маршруты `/hub/*`.
//!
//! Стиль WebSub (W3C), как в hubd: подписка формой с `hub.*`-параметрами,
//! challenge при подписке, `404` от подписчика = «не хочу».

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

use crate::deliver::HubService;
use crate::types::HubError;

pub type SharedService = Arc<HubService>;

/// Роутер хаба: все маршруты под `/hub/*` — единое соглашение узла Pactum.
pub fn hub_router(service: Arc<HubService>) -> Router {
    Router::new()
        .route("/hub/subscribe", post(subscribe))
        .route("/hub/unsubscribe", post(unsubscribe))
        .route("/hub/publish", post(publish))
        .route("/hub/retry", post(retry))
        .route("/hub/subscriptions", get(subscriptions))
        .route("/hub/journal", get(journal))
        .route("/hub/healthz", get(healthz))
        .with_state(service)
}

fn hub_error(err: HubError) -> axum::response::Response {
    let status = match &err {
        HubError::BodyTooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
        HubError::ChallengeFailed => StatusCode::BAD_REQUEST,
        HubError::UnknownSubscription(_) => StatusCode::NOT_FOUND,
        _ => StatusCode::BAD_REQUEST,
    };
    (status, Json(json!({"error": err.to_string()}))).into_response()
}

fn service_error(err: crate::deliver::ServiceError) -> axum::response::Response {
    let status = match &err {
        crate::deliver::ServiceError::Hub(hub_err) => return hub_error(hub_err.clone()),
        crate::deliver::ServiceError::Challenge(_) => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({"error": err.to_string()}))).into_response()
}

/// Параметры подписки: форма WebSub (`hub.mode=subscribe&hub.topic=…`)
/// или JSON с теми же ключами без префикса.
fn param(
    query: &std::collections::HashMap<String, String>,
    body: &Bytes,
    prefixed: &str,
) -> Option<String> {
    if let Some(value) = query.get(prefixed) {
        return Some(value.clone());
    }
    // форма
    if let Ok(form) =
        serde_urlencoded::from_bytes::<std::collections::HashMap<String, String>>(body.as_ref())
    {
        if let Some(value) = form
            .get(prefixed)
            .or_else(|| form.get(&prefixed.replace("hub.", "")))
        {
            return Some(value.clone());
        }
    }
    // json
    if let Ok(json_value) = serde_json::from_slice::<serde_json::Value>(body.as_ref()) {
        let bare = prefixed.replace("hub.", "");
        if let Some(value) = json_value.get(prefixed).or_else(|| json_value.get(&bare)) {
            return value.as_str().map(str::to_string);
        }
    }
    None
}

async fn subscribe(
    State(service): State<SharedService>,
    Query(query): Query<std::collections::HashMap<String, String>>,
    body: Bytes,
) -> axum::response::Response {
    let Some(topic) = param(&query, &body, "hub.topic") else {
        return hub_error(HubError::NoTopic);
    };
    let Some(callback) = param(&query, &body, "hub.callback") else {
        return hub_error(HubError::NoCallback);
    };
    let secret = param(&query, &body, "hub.secret");
    let lease = param(&query, &body, "hub.lease_seconds").and_then(|v| v.parse().ok());
    match service.subscribe(&topic, &callback, secret.as_deref(), lease) {
        Ok(id) => (
            StatusCode::OK,
            Json(json!({"subscription": id, "challenge": "verified"})),
        )
            .into_response(),
        Err(err) => service_error(err),
    }
}

async fn unsubscribe(
    State(service): State<SharedService>,
    Query(query): Query<std::collections::HashMap<String, String>>,
    body: Bytes,
) -> axum::response::Response {
    let Some(topic) = param(&query, &body, "hub.topic") else {
        return hub_error(HubError::NoTopic);
    };
    let Some(callback) = param(&query, &body, "hub.callback") else {
        return hub_error(HubError::NoCallback);
    };
    match service.unsubscribe(&topic, &callback) {
        Ok(()) => (StatusCode::OK, Json(json!({"ok": true}))).into_response(),
        Err(err) => service_error(err),
    }
}

async fn publish(
    State(service): State<SharedService>,
    Query(query): Query<std::collections::HashMap<String, String>>,
    body: Bytes,
) -> axum::response::Response {
    let Some(topic) = query.get("topic").cloned() else {
        return hub_error(HubError::NoTopic);
    };
    match service.publish(&topic, &body) {
        Ok(report) => (
            StatusCode::OK,
            Json(serde_json::to_value(report).unwrap_or_default()),
        )
            .into_response(),
        Err(err) => service_error(err),
    }
}

async fn retry(State(service): State<SharedService>) -> axum::response::Response {
    match service.retry_pass() {
        Ok(report) => (
            StatusCode::OK,
            Json(serde_json::to_value(report).unwrap_or_default()),
        )
            .into_response(),
        Err(err) => service_error(err),
    }
}

async fn subscriptions(State(service): State<SharedService>) -> axum::response::Response {
    let state = match service.snapshot() {
        Ok(state) => state,
        Err(err) => return service_error(err),
    };
    // секреты наружу не выдаём — никогда
    let subs: Vec<serde_json::Value> = state
        .subscriptions
        .iter()
        .map(|s| {
            json!({
                "id": s.id, "topic": s.topic, "callback": s.callback,
                "lease_until": s.lease_until, "state": s.state,
                "has_secret": s.secret.is_some()
            })
        })
        .collect();
    (StatusCode::OK, Json(json!({"subscriptions": subs}))).into_response()
}

async fn journal(State(service): State<SharedService>) -> axum::response::Response {
    let state = match service.snapshot() {
        Ok(state) => state,
        Err(err) => return service_error(err),
    };
    let last: Vec<&crate::types::JournalEntry> = state.journal.iter().rev().take(50).collect();
    (StatusCode::OK, Json(json!({"journal": last}))).into_response()
}

async fn healthz(State(service): State<SharedService>) -> axum::response::Response {
    let state = match service.snapshot() {
        Ok(state) => state,
        Err(err) => return service_error(err),
    };
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "subscriptions": state.subscriptions.len(),
            "pending": state.pending.len()
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deliver::DeliveryOutcome;
    use crate::types::HubConfig;

    struct MockDeliverer;

    impl crate::deliver::Deliverer for MockDeliverer {
        fn deliver(&self, _cb: &str, _t: &str, _b: &[u8], _s: Option<&str>) -> DeliveryOutcome {
            DeliveryOutcome::Delivered
        }
        fn challenge(&self, _cb: &str, _t: &str, _ch: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn app() -> Router {
        hub_router(Arc::new(HubService::new(
            HubConfig::default(),
            Arc::new(MockDeliverer),
        )))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subscribe_publish_flow_over_http() {
        let app = app();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let base = format!("http://{addr}");
        // подписка формой WebSub
        let resp = reqwest_form(
            &base,
            "/hub/subscribe",
            "hub.mode=subscribe&hub.topic=news&hub.callback=http://127.0.0.1:9/cb",
        )
        .await;
        assert_eq!(resp.0, StatusCode::OK);
        // публикация
        let client = ureq::Agent::new();
        let resp = client
            .post(&format!("{base}/hub/publish?topic=news"))
            .send_bytes("первое событие".as_bytes())
            .unwrap();
        let text = resp.into_string().unwrap();
        assert!(text.contains("delivered"));
        // healthz
        let resp = client.get(&format!("{base}/hub/healthz")).call().unwrap();
        let health: serde_json::Value = serde_json::from_str(&resp.into_string().unwrap()).unwrap();
        assert_eq!(health["ok"], serde_json::json!(true));
        assert_eq!(health["subscriptions"], serde_json::json!(1));
    }

    async fn reqwest_form(base: &str, path: &str, form: &str) -> (StatusCode, String) {
        let client = ureq::Agent::new();
        let resp = client
            .post(&format!("{base}{path}"))
            .set("Content-Type", "application/x-www-form-urlencoded")
            .send_string(form)
            .unwrap();
        (StatusCode::OK, resp.into_string().unwrap())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn oversize_publish_returns_413() {
        let app = app();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ureq::Agent::new();
        let result = client
            .post(&format!("http://{addr}/hub/publish?topic=t"))
            .send_bytes(&vec![0u8; 300_000]);
        match result {
            Err(ureq::Error::Status(code, _)) => assert_eq!(code, 413),
            other => panic!("ожидали 413, получено: {other:?}"),
        }
    }
}
