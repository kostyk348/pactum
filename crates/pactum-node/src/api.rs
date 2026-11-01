//! REST API узла — управление и наблюдение.
//!
//! Наследие acp-mvp (api/server.py) в универсальном виде: те же смыслы —
//! входящие, исходящие, escrow, память, репутация.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use pactum_core::letter::Letter;
use serde_json::json;

use crate::orchestrator::{NodeCore, Outcome};

/// Узел + его открытый ключ (для заголовков писем).
pub struct NodeHandle {
    pub core: Arc<NodeCore>,
    pub reply_to: String,
}

impl NodeHandle {
    pub fn new(core: NodeCore, reply_to: String) -> NodeHandle {
        NodeHandle {
            core: Arc::new(core),
            reply_to,
        }
    }
}

pub type SharedNode = Arc<NodeHandle>;

/// Роутер узла: `/inbox`, `/outbox`, `/process`, `/contracts`, `/facts`,
/// `/reputation/{id}`, `/health`. Хаб монтируется отдельно (`hub_router`).
pub fn node_router(handle: SharedNode) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/inbox", get(list_inbox).post(receive_letter))
        .route("/process", post(process))
        .route("/outbox", post(publish))
        .route("/contracts", get(contracts))
        .route("/contracts/{id}", get(contract_by_id))
        .route("/facts", get(facts))
        .route("/facts/verify", get(facts_verify))
        .route("/reputation/{id}", get(reputation))
        .route("/id", get(identity))
        .with_state(handle)
}

async fn health(State(node): State<SharedNode>) -> axum::response::Response {
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "node_id": node.core.node_id(),
            "role": node.core.role,
            "inbox": node.core.inbox_len(),
            "contracts": node.core.known_contracts().len()
        })),
    )
        .into_response()
}

async fn identity(State(node): State<SharedNode>) -> axum::response::Response {
    (
        StatusCode::OK,
        Json(json!({
            "node_id": node.core.node_id(),
            "pubkey": node.core.identity.pubkey_hex(),
            "reply_to": node.reply_to
        })),
    )
        .into_response()
}

async fn list_inbox(State(node): State<SharedNode>) -> axum::response::Response {
    // inbox живёт внутри ядра; наружу даём только счётчик и подсказку
    (
        StatusCode::OK,
        Json(json!({
            "pending": node.core.inbox_len(),
            "note": "письма обрабатываются POST /process"
        })),
    )
        .into_response()
}

/// Сетевой вход узла: текст письма (конверт).
async fn receive_letter(State(node): State<SharedNode>, body: Bytes) -> axum::response::Response {
    let text = String::from_utf8_lossy(&body).to_string();
    match Letter::parse(&text) {
        Ok(letter) => match node.core.receive(letter) {
            Ok(id) => (StatusCode::ACCEPTED, Json(json!({"received": id}))).into_response(),
            Err(err) => (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": err.to_string()})),
            )
                .into_response(),
        },
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

async fn process(State(node): State<SharedNode>) -> axum::response::Response {
    match node.core.process_inbox() {
        Ok(outcomes) => {
            // доказательства, адресованные другому узлу, уходят по reply_to;
            // доставка детерминированная: /process отвечает после отправки
            for outcome in &outcomes {
                if let Outcome::Executed { reply, .. } = outcome {
                    if let Some(target) = reply.header("X-Pactum-Reply-To") {
                        let target = target.to_string();
                        let body = reply.render();
                        tokio::task::spawn_blocking(move || {
                            let agent = ureq::AgentBuilder::new()
                                .timeout_connect(std::time::Duration::from_secs(5))
                                .timeout_read(std::time::Duration::from_secs(10))
                                .build();
                            let result = agent
                                .post(&target)
                                .set("Content-Type", "text/plain; charset=utf-8")
                                .send_string(&body)
                                .map(|_| ())
                                .map_err(|err| err.to_string());
                            if let Err(ref err) = result {
                                eprintln!(
                                    "node: доставка доказательства на {target} не прошла: {err}"
                                );
                            }
                            result
                        })
                        .await
                        .ok();
                    }
                }
            }
            (StatusCode::OK, Json(json!({"outcomes": outcomes}))).into_response()
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

/// Публикация контракта: тело = JSON контракта. Если письмо адресовано
/// релею, CLI/узел перешлёт его сам.
async fn publish(State(node): State<SharedNode>, body: Bytes) -> axum::response::Response {
    let text = String::from_utf8_lossy(&body).to_string();
    match serde_json::from_str::<pactum_contract::Contract>(&text) {
        Ok(contract) => match node.core.publish_contract(contract) {
            Ok(mut letter) => {
                let capability = letter
                    .header("X-Pactum-Capability")
                    .unwrap_or("")
                    .to_string();
                // адрес возврата доказательства: сетевой вход этого узла
                letter.set_header("X-Pactum-Reply-To", &node.reply_to);
                letter.set_header(
                    "X-Pactum-Signature",
                    &node.core.identity.sign(letter.signing_bytes().as_slice()),
                );
                let rendered = letter.render();
                (
                    StatusCode::CREATED,
                    Json(json!({"capability": capability, "letter": rendered})),
                )
                    .into_response()
            }
            Err(err) => (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": err.to_string()})),
            )
                .into_response(),
        },
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("тело не контракт: {err}")})),
        )
            .into_response(),
    }
}

async fn contracts(State(node): State<SharedNode>) -> axum::response::Response {
    let list: Vec<serde_json::Value> = node
        .core
        .known_contracts()
        .iter()
        .map(|c| {
            json!({
                "contract_id": c.contract_id,
                "capability": c.capability(),
                "budget": c.budget_display(),
                "principal": c.parties.principal,
                "executor": c.parties.executor,
                "deadline_utc": c.deadline_utc
            })
        })
        .collect();
    (StatusCode::OK, Json(json!({"contracts": list}))).into_response()
}

async fn contract_by_id(
    State(node): State<SharedNode>,
    Path(id): Path<String>,
) -> axum::response::Response {
    let contract = node
        .core
        .known_contracts()
        .into_iter()
        .find(|c| c.contract_id.as_deref() == Some(id.as_str()));
    match contract {
        Some(contract) => {
            let state = node.core.escrow_state(id.as_str());
            (
                StatusCode::OK,
                Json(json!({"contract": contract, "escrow_state": state.map(|s| s.as_str())})),
            )
                .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "контракт не известен узлу"})),
        )
            .into_response(),
    }
}

async fn facts(State(node): State<SharedNode>) -> axum::response::Response {
    let chain = node
        .core
        .facts
        .lock()
        .map(|c| c.clone())
        .unwrap_or_default();
    (StatusCode::OK, Json(json!({"facts": chain.facts}))).into_response()
}

async fn facts_verify(State(node): State<SharedNode>) -> axum::response::Response {
    match node.core.verify_memory() {
        Ok(()) => (StatusCode::OK, Json(json!({"verified": true}))).into_response(),
        Err(err) => (
            StatusCode::CONFLICT,
            Json(json!({"verified": false, "error": err.to_string()})),
        )
            .into_response(),
    }
}

async fn reputation(
    State(node): State<SharedNode>,
    Path(id): Path<String>,
) -> axum::response::Response {
    (
        StatusCode::OK,
        Json(json!({"node": id, "reputation": node.core.reputation(id.as_str())})),
    )
        .into_response()
}

/// Хелпер для тестов и CLI: поднять узел на свободном порту.
pub async fn serve_node(
    handle: SharedNode,
) -> std::io::Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let app = node_router(handle);
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((addr, task))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::{NodeCore, NodeOptions, Role};
    use pactum_identity::KeyPair;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn health_and_facts_over_http() {
        let core = NodeCore::new(KeyPair::generate(), Role::Principal, NodeOptions::default());
        core.record_fact(pactum_core::facts::Register::Action, "узел поднялся", None);
        let handle = Arc::new(NodeHandle::new(core, "http://127.0.0.1:1/inbox".into()));
        let (addr, _task) = serve_node(handle).await.unwrap();

        let client = ureq::Agent::new();
        let health: serde_json::Value = serde_json::from_str(
            &client
                .get(&format!("http://{addr}/health"))
                .call()
                .unwrap()
                .into_string()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(health["ok"], serde_json::json!(true));

        let facts: serde_json::Value = serde_json::from_str(
            &client
                .get(&format!("http://{addr}/facts"))
                .call()
                .unwrap()
                .into_string()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(facts["facts"].as_array().unwrap().len(), 1);

        let verify: serde_json::Value = serde_json::from_str(
            &client
                .get(&format!("http://{addr}/facts/verify"))
                .call()
                .unwrap()
                .into_string()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(verify["verified"], serde_json::json!(true));
    }
}
