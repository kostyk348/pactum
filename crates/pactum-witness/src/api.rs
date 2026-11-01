//! HTTP API нотариуса. Отказы честные и машиночитаемые:
//! `400` — плохой запрос, `404` — «документ не нотариован», `413` —
//! «тело слишком большое», `429` — «слишком часто», `502` —
//! «источник недоступен». Никаких молчаливых потерь.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

use crate::error::WitnessError;
use crate::receipt::RECEIPT_V;
use crate::service::{WitnessService, NOTE_MAX_CHARS};

pub const MEDIA_MAX_CHARS: usize = 128;
pub const DIGEST_LEN: usize = 64;

/// Общее состояние сервиса.
pub type WitnessState = Arc<WitnessService>;

/// Роутер нотариуса. Потолок тела берётся из настроек сервиса.
pub fn witness_router(state: WitnessState) -> Router {
    let limit = state.config().max_body;
    Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/id", get(identity))
        .route("/notarize", post(notarize))
        .route("/notarize/url", post(notarize_url))
        .route("/receipts", get(list_receipts))
        .route("/receipts/{digest}", get(get_receipt))
        .route("/verify/{digest}", get(verify))
        .route("/chain/head", get(chain_head))
        .route("/chain/export", get(chain_export))
        .route("/blobs/{digest}", get(get_blob))
        .layer(axum::extract::DefaultBodyLimit::max(limit))
        .with_state(state)
}

/// Поднять сервис на адресе (для CLI и тестов).
pub async fn serve_witness(
    state: WitnessState,
    addr: &str,
) -> std::io::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let app = witness_router(state);
    let task = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    Ok((local, task))
}

// ---- GET ----

async fn index() -> axum::response::Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        INDEX_HTML,
    )
        .into_response()
}

async fn health(State(s): State<WitnessState>) -> axum::response::Response {
    let summary = s
        .summary()
        .unwrap_or_else(|_| json!({"error": "журнал не читается"}));
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "service": "pactum-witness",
            "protocol": "acp/1",
            "receipt_version": RECEIPT_V,
            "witness": s.node_id(),
            "pubkey": s.pubkey_hex(),
            "journal": summary,
        })),
    )
        .into_response()
}

async fn identity(State(s): State<WitnessState>) -> axum::response::Response {
    (
        StatusCode::OK,
        Json(json!({
            "witness": s.node_id(),
            "pubkey": s.pubkey_hex(),
            "scheme": "ecdsa-p256",
        })),
    )
        .into_response()
}

async fn list_receipts(
    State(s): State<WitnessState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(50)
        .min(1000);
    let offset = params
        .get("offset")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    match s.list(offset, limit) {
        Ok((total, receipts)) => (
            StatusCode::OK,
            Json(json!({"total": total, "offset": offset, "receipts": receipts})),
        )
            .into_response(),
        Err(err) => service_error(&err),
    }
}

async fn get_receipt(
    State(s): State<WitnessState>,
    Path(digest): Path<String>,
) -> axum::response::Response {
    if !is_digest(&digest) {
        return bad_digest();
    }
    match s.verified_receipt(&digest) {
        Ok(receipt) => (StatusCode::OK, Json(json!({"receipt": receipt}))).into_response(),
        Err(WitnessError::NotFound(_)) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "документ с таким SHA-256 не нотариован"})),
        )
            .into_response(),
        Err(err) => service_error(&err),
    }
}

async fn verify(
    State(s): State<WitnessState>,
    Path(digest): Path<String>,
) -> axum::response::Response {
    if !is_digest(&digest) {
        return bad_digest();
    }
    let receipt = match s.verified_receipt(&digest) {
        Ok(receipt) => receipt,
        Err(WitnessError::NotFound(_)) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "документ с таким SHA-256 не нотариован"})),
            )
                .into_response()
        }
        Err(err) => return service_error(&err),
    };
    let head = s.signed_head().ok().flatten();
    (
        StatusCode::OK,
        Json(json!({
            "digest": digest,
            "verified": true,
            "receipt": receipt,
            "head": head,
            "pubkey": s.pubkey_hex(),
            "how": "офлайн: sha256(каноническая строка приёмника) → ecdsa-p256 по pubkey",
        })),
    )
        .into_response()
}

async fn chain_head(State(s): State<WitnessState>) -> axum::response::Response {
    match s.signed_head() {
        Ok(Some(head)) => (
            StatusCode::OK,
            Json(json!({"head": head, "pubkey": s.pubkey_hex()})),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::OK,
            Json(json!({"head": serde_json::Value::Null, "note": "журнал пуст"})),
        )
            .into_response(),
        Err(err) => service_error(&err),
    }
}

async fn chain_export(State(s): State<WitnessState>) -> axum::response::Response {
    match s.export_jsonl() {
        Ok(jsonl) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/x-ndjson; charset=utf-8")],
            jsonl,
        )
            .into_response(),
        Err(err) => service_error(&err),
    }
}

async fn get_blob(
    State(s): State<WitnessState>,
    Path(digest): Path<String>,
) -> axum::response::Response {
    if !is_digest(&digest) {
        return bad_digest();
    }
    match s.blob(&digest) {
        Ok(Some(bytes)) => {
            let media = s
                .verified_receipt(&digest)
                .ok()
                .and_then(|r| r.media_type)
                .unwrap_or_else(|| "application/octet-stream".into());
            (StatusCode::OK, [(header::CONTENT_TYPE, media)], bytes).into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": "содержимое не сохранялось: notarize без store=1 хранит только хеш",
                "receipt": s.verified_receipt(&digest).ok(),
            })),
        )
            .into_response(),
        Err(WitnessError::NotFound(_)) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "документ с таким SHA-256 не нотариован"})),
        )
            .into_response(),
        Err(err) => service_error(&err),
    }
}

// ---- POST ----

async fn notarize(
    State(s): State<WitnessState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    body: Bytes,
) -> axum::response::Response {
    if !s.check_rate(peer.ip()) {
        return too_many_requests();
    }
    let max = s.config().max_body;
    if body.len() > max {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({"error": format!("тело больше лимита {max} байт")})),
        )
            .into_response();
    }
    let media = match optional_param(&params, "media", MEDIA_MAX_CHARS) {
        Ok(media) => media,
        Err(err) => return err.into_response(),
    };
    let note = match optional_param(&params, "note", NOTE_MAX_CHARS) {
        Ok(note) => note,
        Err(err) => return err.into_response(),
    };
    let store = store_flag(&params);
    let outcome = tokio::task::spawn_blocking(move || s.notarize(&body, media, note, store))
        .await
        .unwrap_or_else(|_| Err(join_error()));
    match outcome {
        Ok(out) if out.duplicate => (
            StatusCode::OK,
            Json(json!({
                "receipt": out.receipt,
                "duplicate": true,
                "note": "такой дайджест уже нотариован — приёмник прежний, журнал не растёт",
            })),
        )
            .into_response(),
        Ok(out) => (
            StatusCode::CREATED,
            Json(json!({
                "receipt": out.receipt,
                "duplicate": false,
            })),
        )
            .into_response(),
        Err(err) => service_error(&err),
    }
}

async fn notarize_url(
    State(s): State<WitnessState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    if !s.check_rate(peer.ip()) {
        return too_many_requests();
    }
    let Some(url) = params.get("url").cloned() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "нужен параметр url"})),
        )
            .into_response();
    };
    let store = store_flag(&params);
    let outcome = tokio::task::spawn_blocking(move || s.notarize_url(&url, store))
        .await
        .unwrap_or_else(|_| Err(join_error()));
    match outcome {
        Ok(out) if out.duplicate => (
            StatusCode::OK,
            Json(json!({"receipt": out.receipt, "duplicate": true})),
        )
            .into_response(),
        Ok(out) => (
            StatusCode::CREATED,
            Json(json!({"receipt": out.receipt, "duplicate": false})),
        )
            .into_response(),
        Err(err) => service_error(&err),
    }
}

// ---- хелперы ----

fn store_flag(params: &std::collections::HashMap<String, String>) -> bool {
    matches!(
        params.get("store").map(String::as_str),
        Some("1" | "true" | "yes")
    )
}

type ParamError = (StatusCode, Json<serde_json::Value>);

fn optional_param(
    params: &std::collections::HashMap<String, String>,
    key: &str,
    max_chars: usize,
) -> Result<Option<String>, ParamError> {
    match params.get(key) {
        None => Ok(None),
        Some(value) if value.chars().count() <= max_chars => Ok(Some(value.clone())),
        Some(_) => Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("параметр {key} длиннее {max_chars} символов")})),
        )),
    }
}

fn is_digest(text: &str) -> bool {
    text.len() == DIGEST_LEN && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn bad_digest() -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": "digest должен быть 64 hex-символа (SHA-256)"})),
    )
        .into_response()
}

fn too_many_requests() -> axum::response::Response {
    let mut resp = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({"error": "слишком часто: лимит окна не исчерпан — подождите минуту"})),
    )
        .into_response();
    resp.headers_mut()
        .insert(header::RETRY_AFTER, "60".parse().unwrap());
    resp
}

fn join_error() -> WitnessError {
    WitnessError::Network("внутренняя задача прервана".into())
}

fn service_error(err: &WitnessError) -> axum::response::Response {
    let status = match err {
        WitnessError::NotFound(_) => StatusCode::NOT_FOUND,
        WitnessError::BadUrl(_) => StatusCode::BAD_REQUEST,
        WitnessError::TooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
        WitnessError::Upstream { .. }
        | WitnessError::Network(_)
        | WitnessError::TooManyRedirects(_) => StatusCode::BAD_GATEWAY,
        WitnessError::Signature(_)
        | WitnessError::Hash(_)
        | WitnessError::Identity(_)
        | WitnessError::ChainBroken(_, _) => StatusCode::CONFLICT,
        WitnessError::Io(_) | WitnessError::Json(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({"error": err.to_string()}))).into_response()
}

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="ru"><head><meta charset="utf-8"><title>Pactum Witness</title>
<style>
body{font-family:ui-monospace,monospace;background:#0d1420;color:#dbe4f0;max-width:52rem;margin:3rem auto;padding:0 1rem;line-height:1.6}
h1{color:#f0b429}code{background:#182338;padding:.1rem .35rem;border-radius:4px;color:#9fd3a8}
table{border-collapse:collapse;width:100%}td{border-top:1px solid #22314d;padding:.35rem .5rem;vertical-align:top}
</style></head><body>
<h1>Pactum Witness</h1>
<p>Нотариус контента по протоколу ACP/1: подписанный приёмник на каждый документ, журнал, связанный хешами, проверка офлайн.</p>
<p>A content notary: a signed receipt per document, a hash-chained journal, offline verification.</p>
<table>
<tr><td><code>POST /notarize?store=1&amp;media=…&amp;note=…</code></td><td>тело = документ → приёмник. Body = document → receipt.</td></tr>
<tr><td><code>POST /notarize/url?url=…</code></td><td>скачать и нотаризовать страницу. Fetch and notarize a page.</td></tr>
<tr><td><code>GET /receipts/{sha256}</code></td><td>приёмник. The receipt.</td></tr>
<tr><td><code>GET /verify/{sha256}</code></td><td>приёмник + подписанная голова. Receipt + signed head.</td></tr>
<tr><td><code>GET /chain/head</code></td><td>checkpoint. Signed chain head.</td></tr>
<tr><td><code>GET /chain/export</code></td><td>весь журнал NDJSON. Whole journal.</td></tr>
<tr><td><code>GET /blobs/{sha256}</code></td><td>сохранённое содержимое (store=1). Stored bytes.</td></tr>
<tr><td><code>GET /health</code></td><td>статус и открытый ключ. Status and pubkey.</td></tr>
</table>
<p><a style="color:#7aa2f7" href="/health">/health</a></p>
</body></html>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::WitnessConfig;
    use pactum_identity::KeyPair;
    use std::path::PathBuf;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pactum-wapi-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn spawn(tag: &str, cfg: WitnessConfig) -> (String, WitnessState, PathBuf) {
        let dir = tmpdir(tag);
        let key = KeyPair::generate();
        let service = WitnessService::open(&dir, key, cfg).unwrap();
        let state: WitnessState = Arc::new(service);
        let (addr, _task) = serve_witness(state.clone(), "127.0.0.1:0").await.unwrap();
        (format!("http://{addr}"), state, dir)
    }

    fn get_json(base: &str, path: &str) -> (u16, serde_json::Value) {
        let resp = ureq::get(&format!("{base}{path}")).call();
        status_body(resp)
    }

    fn post_bytes(base: &str, path: &str, body: &[u8]) -> (u16, serde_json::Value) {
        let resp = ureq::post(&format!("{base}{path}"))
            .set("Content-Type", "application/octet-stream")
            .send_bytes(body);
        status_body(resp)
    }

    fn status_body(resp: Result<ureq::Response, ureq::Error>) -> (u16, serde_json::Value) {
        match resp {
            Ok(r) => {
                let status = r.status();
                let text = r.into_string().unwrap_or_default();
                (status, serde_json::from_str(&text).unwrap_or_default())
            }
            Err(ureq::Error::Status(status, r)) => {
                let text = r.into_string().unwrap_or_default();
                (status, serde_json::from_str(&text).unwrap_or_default())
            }
            Err(err) => panic!("сеть отвалилась в тесте: {err}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn notarize_receipt_verify_blob_flow() {
        let (base, _state, _dir) = spawn("flow", WitnessConfig::default()).await;

        let (status, body) = post_bytes(
            &base,
            "/notarize?store=1&media=text/plain&note=первый",
            "документ №1".as_bytes(),
        );
        assert_eq!(status, 201, "{body:?}");
        let receipt = body["receipt"].clone();
        let digest = receipt["content_sha256"].as_str().unwrap().to_string();
        assert_eq!(digest.len(), 64);
        assert_eq!(body["duplicate"], serde_json::json!(false));

        // дубликат: 200 + тот же приёмник
        let (status, body) = post_bytes(&base, "/notarize", "документ №1".as_bytes());
        assert_eq!(status, 200);
        assert_eq!(body["duplicate"], serde_json::json!(true));
        assert_eq!(body["receipt"]["seq"], receipt["seq"]);

        // приёмник и проверка
        let (status, body) = get_json(&base, &format!("/receipts/{digest}"));
        assert_eq!(status, 200);
        assert_eq!(body["receipt"]["content_sha256"], receipt["content_sha256"]);

        let (status, body) = get_json(&base, &format!("/verify/{digest}"));
        assert_eq!(status, 200);
        assert_eq!(body["verified"], serde_json::json!(true));
        assert!(body["head"]["sig"]
            .as_str()
            .unwrap()
            .starts_with("ecdsa-p256:"));

        // сохранённый blob отдаётся байт в байт
        let resp = ureq::get(&format!("{base}/blobs/{digest}")).call().unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.into_string().unwrap(), "документ №1");

        // журнал и голова
        let (status, body) = get_json(&base, "/chain/head");
        assert_eq!(status, 200);
        assert_eq!(body["head"]["seq"], serde_json::json!(1));

        let (status, body) = get_json(&base, "/receipts?limit=10");
        assert_eq!(status, 200);
        assert_eq!(body["total"], serde_json::json!(1));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn honest_refusals() {
        let cfg = WitnessConfig {
            url_limits: crate::fetch::UrlLimits {
                allow_private: true,
                ..crate::fetch::UrlLimits::default()
            },
            ..WitnessConfig::default()
        };
        let (base, _state, _dir) = spawn("refuse", cfg).await;

        // локальный источник с ошибкой 500 (песочница не резолвит внешние IP)
        let upstream_status = Arc::new(std::sync::atomic::AtomicU16::new(0));
        let marker = upstream_status.clone();
        let app = axum::Router::new().route(
            "/boom",
            axum::routing::get(move || {
                marker.store(500, std::sync::atomic::Ordering::SeqCst);
                async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "упс") }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        // неизвестный digest — честный 404
        let unknown = "0".repeat(64);
        let (status, body) = get_json(&base, &format!("/receipts/{unknown}"));
        assert_eq!(status, 404);
        assert!(body["error"].as_str().unwrap().contains("не нотариован"));

        // кривой digest — 400
        let (status, _) = get_json(&base, "/receipts/abc");
        assert_eq!(status, 400);

        // blob без store=1 — 404 с объяснением
        let (status, body) = post_bytes(&base, "/notarize", "без сохранения".as_bytes());
        assert_eq!(status, 201);
        let digest = body["receipt"]["content_sha256"]
            .as_str()
            .unwrap()
            .to_string();
        let (status, body) = get_json(&base, &format!("/blobs/{digest}"));
        assert_eq!(status, 404);
        assert!(body["error"].as_str().unwrap().contains("store=1"));

        // плохая схема url — 400
        let (status, _) = post_bytes(&base, "/notarize/url?url=file:///etc/passwd", b"");
        assert_eq!(status, 400);

        // приватный адрес — 400 (SSRF): отдельный сервис с настройками по умолчанию
        let (ssrf, _state2, _dir2) = spawn("ssrf", WitnessConfig::default()).await;
        let (status, body) = post_bytes(&ssrf, "/notarize/url?url=http://127.0.0.1:9/x", b"");
        assert_eq!(status, 400, "{body:?}");
        assert!(body["error"].as_str().unwrap().contains("приватный"));

        // источник отвечает 500 — честный 502 наверх
        let (status, _) = post_bytes(
            &base,
            &format!("/notarize/url?url=http://{origin}/boom"),
            b"",
        );
        assert_eq!(status, 502);
        assert_eq!(
            upstream_status.load(std::sync::atomic::Ordering::SeqCst),
            500
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn oversize_body_is_413() {
        let cfg = WitnessConfig {
            max_body: 1_000,
            url_limits: crate::fetch::UrlLimits {
                allow_private: true,
                max_bytes: 1_000,
                ..crate::fetch::UrlLimits::default()
            },
            ..WitnessConfig::default()
        };
        let (base, _state, _dir) = spawn("big", cfg).await;
        let big = vec![0u8; 1_500];
        let (status, body) = post_bytes(&base, "/notarize", &big);
        assert_eq!(status, 413, "{body:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rate_limit_is_429_with_retry_after() {
        let cfg = WitnessConfig {
            rate_per_min: 1,
            ..WitnessConfig::default()
        };
        let (base, _state, _dir) = spawn("rate", cfg).await;
        let (status, _) = post_bytes(&base, "/notarize", "первый".as_bytes());
        assert_eq!(status, 201);
        let resp = ureq::post(&format!("{base}/notarize")).send_bytes("второй".as_bytes());
        match resp {
            Err(ureq::Error::Status(status, r)) => {
                assert_eq!(status, 429);
                assert_eq!(r.header("Retry-After"), Some("60"));
            }
            other => panic!("ожидали 429, получено: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn url_notarize_happy_path_and_blob() {
        let cfg = WitnessConfig {
            url_limits: crate::fetch::UrlLimits {
                allow_private: true,
                ..crate::fetch::UrlLimits::default()
            },
            ..WitnessConfig::default()
        };
        let (base, _state, _dir) = spawn("url", cfg).await;

        // локальный источник-страница
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let origin = std::thread::spawn(move || {
            let addr = rt.block_on(async {
                let app = axum::Router::new().route(
                    "/page",
                    axum::routing::get(|| async { "страница новостей v1" }),
                );
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                tokio::spawn(async move {
                    let _ = axum::serve(listener, app).await;
                });
                addr
            });
            std::mem::forget(rt);
            addr
        })
        .join()
        .unwrap();

        let (status, body) = post_bytes(
            &base,
            &format!("/notarize/url?url=http://{origin}/page&store=1"),
            b"",
        );
        assert_eq!(status, 201, "{body:?}");
        let digest = body["receipt"]["content_sha256"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            digest,
            pactum_core::hash::sha256_text("страница новостей v1")
        );
        let resp = ureq::get(&format!("{base}/blobs/{digest}")).call().unwrap();
        assert_eq!(resp.into_string().unwrap(), "страница новостей v1");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn checkpoint_publishes_to_mock_hub() {
        // мок хаба: ловим POST /hub/publish
        let captured: Arc<std::sync::Mutex<Vec<(String, String)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = captured.clone();
        let app = axum::Router::new().route(
            "/hub/publish",
            post(
                move |Query(q): Query<std::collections::HashMap<String, String>>, body: Bytes| {
                    let sink = sink.clone();
                    async move {
                        sink.lock().unwrap().push((
                            q.get("topic").cloned().unwrap_or_default(),
                            String::from_utf8_lossy(&body).to_string(),
                        ));
                        "ok"
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hub_addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let dir = tmpdir("hub");
        let cfg = WitnessConfig {
            hub_url: Some(format!("http://{hub_addr}")),
            checkpoint_every: 1,
            ..WitnessConfig::default()
        };
        let state: WitnessState =
            Arc::new(WitnessService::open(&dir, KeyPair::generate(), cfg).unwrap());
        state
            .notarize("первый в хаб".as_bytes(), None, None, false)
            .unwrap();
        state.publish_checkpoint_now().unwrap();

        let got = captured.lock().unwrap();
        // первый checkpoint — из notarize (every=1), второй — явный вызов
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got.iter().all(|(topic, _)| topic == "witness.head"));
        assert!(got
            .iter()
            .all(|(_, body)| body.contains("witness.checkpoint")));
        assert!(got[0].1.contains("head_hash"));
    }
}
