//! `pactum node` — узел сети: REST API + хаб + relay в одном процессе.

use clap::Subcommand;
use pactum_identity::{load_keystore, KeyPair};
use pactum_node::api::{node_router, NodeHandle};
use pactum_node::orchestrator::{NodeCore, NodeOptions, Role};
use pactum_node::relay::{relay_router, RelayRegistry, RelaySubscription};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Subcommand)]
pub enum NodeCommand {
    /// Поднять узел: /inbox, /outbox, /process, /hub/*, /relay/*
    Serve {
        #[arg(long, default_value_t = 8080)]
        port: u16,
        /// Роль: principal (заказчик) или executor (исполнитель)
        #[arg(long, default_value = "executor")]
        role: String,
        /// Capability исполнителя через запятую (wildcard * допустим)
        #[arg(long, default_value = "*")]
        capabilities: String,
        /// Файл keystore (генерируется, если нет)
        #[arg(long, default_value = "pactum-key.json")]
        keystore: PathBuf,
        /// Парольная фраза keystore (env PACTUM_PASSPHRASE)
        #[arg(long, env = "PACTUM_PASSPHRASE")]
        passphrase: Option<String>,
        /// URL публичного relay: подписаться своими capability
        #[arg(long)]
        relay: Option<String>,
        /// Файл памяти узла (цепочка фактов)
        #[arg(long, default_value = "data/facts.json")]
        facts: PathBuf,
        /// Минимальный бюджет, который узел берёт (антиспам)
        #[arg(long, default_value_t = 0.01)]
        min_budget: f64,
    },
}

pub fn run(command: NodeCommand) -> i32 {
    match command {
        NodeCommand::Serve {
            port,
            role,
            capabilities,
            keystore,
            passphrase,
            relay,
            facts,
            min_budget,
        } => {
            let role = match role.as_str() {
                "principal" | "заказчик" => Role::Principal,
                "executor" | "исполнитель" => Role::Executor,
                other => {
                    eprintln!("роль не распознана: {other} (principal | executor)");
                    return 2;
                }
            };
            // ключ: из keystore или новый (честно предупредив)
            let key = if keystore.exists() {
                match load_keystore(&keystore, passphrase.as_deref()) {
                    Ok(key) => key,
                    Err(err) => {
                        eprintln!("keystore не прочитан: {err}");
                        return 1;
                    }
                }
            } else {
                let key = KeyPair::generate();
                if pactum_identity::save_keystore(&keystore, &key, passphrase.as_deref()).is_err() {
                    eprintln!("keystore не сохранён");
                    return 1;
                }
                if passphrase.is_none() {
                    eprintln!(
                        "внимание: ключ сохранён без шифрования — задайте --passphrase для AES-GCM"
                    );
                }
                key
            };

            let capabilities: Vec<String> = capabilities
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            let options = NodeOptions {
                capabilities,
                min_budget_units: min_budget,
                workdir: std::env::temp_dir().join(format!("pactum-node-{}", key.node_id())),
            };
            let core = NodeCore::new(key, role, options);
            // восстановить память узла, если она есть
            if facts.exists() {
                match pactum_core::facts::FactChain::load(&facts) {
                    Ok(chain) => {
                        if let Ok(mut slot) = core.facts.lock() {
                            *slot = chain;
                        }
                    }
                    Err(err) => {
                        eprintln!(
                            "внимание: память повреждена, узел начинает новую цепочку: {err}"
                        );
                    }
                }
            }

            let runtime = tokio::runtime::Runtime::new().expect("runtime");
            runtime.block_on(async move {
                let listener =
                    match tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port)))
                        .await
                    {
                        Ok(listener) => listener,
                        Err(err) => {
                            eprintln!("порт {port} не занялся: {err}");
                            return 1;
                        }
                    };
                let addr = listener.local_addr().unwrap();
                let inbox_url = format!("http://{addr}/inbox");
                let node = Arc::new(NodeHandle::new(core, inbox_url.clone()));
                let node_id = node.core.node_id();

                // подписка на relay, если задан
                if let Some(relay_url) = relay {
                    let sub = RelaySubscription {
                        node_id: node_id.clone(),
                        capability: node
                            .core
                            .options
                            .capabilities
                            .join(",")
                            .split(',')
                            .next()
                            .unwrap_or("*")
                            .to_string(),
                        endpoint: inbox_url.clone(),
                    };
                    let client = ureq::Agent::new();
                    match client
                        .post(&format!("{relay_url}/relay/subscribe"))
                        .send_json(serde_json::to_value(&sub).unwrap_or_default())
                    {
                        Ok(_) => println!("node: подписан на relay {relay_url}"),
                        Err(err) => eprintln!("node: relay не принял подписку: {err}"),
                    }
                }

                println!(
                    "node: {} ({})",
                    node_id,
                    if role == Role::Principal {
                        "заказчик"
                    } else {
                        "исполнитель"
                    }
                );
                println!("node: API      http://{addr}");
                println!("node: inbox    {inbox_url}");
                println!("node: hub      http://{addr}/hub/*");
                println!("node: relay    http://{addr}/relay/*");
                println!("node: память   {}", facts.display());

                let relay_registry: Arc<RelayRegistry> = Arc::new(RelayRegistry::new());
                let app = node_router(node.clone())
                    .merge(relay_router(relay_registry))
                    .merge(pactum_hub::hub_router(Arc::new(
                        pactum_hub::deliver::HubService::new(
                            pactum_hub::HubConfig::default(),
                            Arc::new(pactum_hub::deliver::HttpDeliverer::default()),
                        ),
                    )));

                // обслуживание: слэшинг просроченных сделок
                let node_for_tick = node.clone();
                tokio::spawn(async move {
                    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(10));
                    loop {
                        ticker.tick().await;
                        if let Ok(mut engine) = node_for_tick.core.engine.lock() {
                            let slashed = engine.tick(pactum_core::time::now());
                            for id in slashed {
                                eprintln!("node: слэшинг по тайм-ауту: {id}");
                            }
                        }
                    }
                });

                // периодическая запись памяти на диск
                let persist_facts = facts;
                let node_for_persist = node.clone();
                tokio::spawn(async move {
                    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
                    loop {
                        ticker.tick().await;
                        if let Ok(chain) = node_for_persist.core.facts.lock() {
                            let _ = chain.save(&persist_facts);
                        }
                    }
                });

                if let Err(err) = axum::serve(listener, app).await {
                    eprintln!("сервер упал: {err}");
                    return 1;
                }
                0
            })
        }
    }
}
