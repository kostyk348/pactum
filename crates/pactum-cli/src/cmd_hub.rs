//! `pactum hub` — сервер и клиенты хаба подписок.

use clap::Subcommand;
use pactum_hub::deliver::{HttpDeliverer, HubService};
use pactum_hub::{hub_router, HubConfig};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Subcommand)]
pub enum HubCommand {
    /// Поднять хаб: подписки, публикации, ретраи, честные отказы
    Serve {
        #[arg(long, default_value_t = 8765)]
        port: u16,
        /// Файл состояния (подписки + очередь ретраев переживают рестарт)
        #[arg(long, default_value = "data/hub.json")]
        store: PathBuf,
        /// Лимит тела сообщения, байт
        #[arg(long, default_value_t = 256 * 1024)]
        max_body: usize,
    },
    /// Подписаться на топик
    Subscribe {
        /// Адрес хаба, например http://127.0.0.1:8765
        #[arg(long)]
        hub: String,
        #[arg(long)]
        topic: String,
        /// URL подписчика (его входная точка)
        #[arg(long)]
        callback: String,
        #[arg(long)]
        secret: Option<String>,
        /// Аренда, секунд (максимум 86400)
        #[arg(long)]
        lease: Option<u64>,
    },
    /// Опубликовать событие в топик
    Publish {
        #[arg(long)]
        hub: String,
        #[arg(long)]
        topic: String,
        /// Текст события
        #[arg(long)]
        text: Option<String>,
        /// Прочитать событие из файла
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Один проход по очереди ретраев
    Retry {
        #[arg(long)]
        hub: String,
    },
    /// Подписки хаба (без секретов)
    Subscriptions {
        #[arg(long)]
        hub: String,
    },
    /// Последние события журнала
    Journal {
        #[arg(long)]
        hub: String,
    },
    /// Здоровье хаба
    Health {
        #[arg(long)]
        hub: String,
    },
    /// Локальный слушатель-подписчик: эхо challenge + приём доставок
    Listen {
        #[arg(long, default_value_t = 8766)]
        port: u16,
        /// Ожидаемая подпись доставок (X-Hub-Signature)
        #[arg(long)]
        secret: Option<String>,
    },
}

fn get_json(url: &str) -> Result<serde_json::Value, String> {
    let agent = ureq::Agent::new();
    let resp = agent.get(url).call().map_err(|err| err.to_string())?;
    resp.into_string()
        .map_err(|err| err.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|err| err.to_string()))
}

fn post_text(url: &str, body: &str) -> Result<serde_json::Value, String> {
    let agent = ureq::Agent::new();
    let resp = agent
        .post(url)
        .set("Content-Type", "application/octet-stream")
        .send_string(body)
        .map_err(|err| match err {
            ureq::Error::Status(code, resp) => {
                let detail = resp.into_string().unwrap_or_default();
                format!("HTTP {code}: {detail}")
            }
            other => other.to_string(),
        })?;
    resp.into_string()
        .map_err(|err| err.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|err| err.to_string()))
}

async fn serve_hub(port: u16, store: PathBuf, max_body: usize) -> i32 {
    let config = HubConfig {
        max_body,
        store_path: Some(store.clone()),
        ..Default::default()
    };
    let state = pactum_hub::load_state(&store).unwrap_or_default();
    let service = Arc::new(HubService::from_state(
        state,
        config,
        Arc::new(HttpDeliverer::default()),
    ));
    // фоновый цикл: ретраи и снятие истёкших аренд
    {
        let service = service.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                ticker.tick().await;
                let _ = service.retry_pass();
                let _ = service.expire_leases();
            }
        });
    }
    let app = hub_router(service);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    println!("hub: слушаю http://{addr}");
    println!("hub: состояние в {}", store.display());
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("порт {port} не занялся: {err}");
            return 1;
        }
    };
    if let Err(err) = axum::serve(listener, app).await {
        eprintln!("сервер упал: {err}");
        return 1;
    }
    0
}

async fn listen_subscriber(port: u16, secret: Option<String>) -> i32 {
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::get;
    use axum::Router;

    let secret = Arc::new(secret);
    let app = Router::new()
        .route(
            "/",
            get(|| async { "pactum-listener" }).post(
                |State(secret): State<Arc<Option<String>>>, headers: HeaderMap, body: Bytes| async move {
                    if let Some(expected) = secret.as_deref() {
                        let got = headers
                            .get("X-Hub-Signature")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("");
                        let want = pactum_hub::sig::hub_signature(expected, &body);
                        if got != want {
                            eprintln!("подпись не сошлась: получено {got}, ожидалось {want}");
                            return StatusCode::FORBIDDEN;
                        }
                    }
                    let text = String::from_utf8_lossy(&body);
                    println!("доставка ({} байт): {text}", body.len());
                    StatusCode::OK
                },
            ),
        )
        .with_state(secret);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    println!("listener: http://{addr} (callback: http://{addr}/)");
    println!("listener: отвечаю на challenge автоматически");
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("порт {port} не занялся: {err}");
            return 1;
        }
    };
    if let Err(err) = axum::serve(listener, app).await {
        eprintln!("сервер упал: {err}");
        return 1;
    }
    0
}

pub fn run(command: HubCommand) -> i32 {
    match command {
        HubCommand::Serve {
            port,
            store,
            max_body,
        } => {
            let runtime = tokio::runtime::Runtime::new().expect("runtime");
            runtime.block_on(serve_hub(port, store, max_body))
        }
        HubCommand::Listen { port, secret } => {
            let runtime = tokio::runtime::Runtime::new().expect("runtime");
            runtime.block_on(listen_subscriber(port, secret))
        }
        HubCommand::Subscribe {
            hub,
            topic,
            callback,
            secret,
            lease,
        } => {
            // challenge: слушатель сам подтвердит; здесь шлём форму WebSub
            let mut form = format!(
                "hub.mode=subscribe&hub.topic={}&hub.callback={}",
                urlencode(&topic),
                urlencode(&callback)
            );
            if let Some(secret) = &secret {
                form.push_str(&format!("&hub.secret={}", urlencode(secret)));
            }
            if let Some(lease) = lease {
                form.push_str(&format!("&hub.lease_seconds={lease}"));
            }
            match post_text(&format!("{hub}/hub/subscribe"), &form) {
                Ok(resp) => {
                    println!("подписка принята: {resp}");
                    0
                }
                Err(err) => {
                    eprintln!("подписка не принята: {err}");
                    1
                }
            }
        }
        HubCommand::Publish {
            hub,
            topic,
            text,
            file,
        } => {
            let body = if let Some(path) = file {
                std::fs::read_to_string(&path).unwrap_or_default()
            } else {
                text.unwrap_or_default()
            };
            if body.is_empty() {
                eprintln!("пустое событие: задайте --text или --file");
                return 2;
            }
            match post_text(
                &format!("{hub}/hub/publish?topic={}", urlencode(&topic)),
                &body,
            ) {
                Ok(resp) => {
                    println!("опубликовано: {resp}");
                    0
                }
                Err(err) => {
                    eprintln!("публикация не прошла: {err}");
                    1
                }
            }
        }
        HubCommand::Retry { hub } => match post_text(&format!("{hub}/hub/retry"), "") {
            Ok(resp) => {
                println!("ретраи: {resp}");
                0
            }
            Err(err) => {
                eprintln!("ретрай не прошёл: {err}");
                1
            }
        },
        HubCommand::Subscriptions { hub } => print_json(&format!("{hub}/hub/subscriptions")),
        HubCommand::Journal { hub } => print_json(&format!("{hub}/hub/journal")),
        HubCommand::Health { hub } => print_json(&format!("{hub}/hub/healthz")),
    }
}

fn print_json(url: &str) -> i32 {
    match get_json(url) {
        Ok(value) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_default()
            );
            0
        }
        Err(err) => {
            eprintln!("запрос не прошёл: {err}");
            1
        }
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
