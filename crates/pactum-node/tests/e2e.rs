//! Сквозной тест сети ACP: два узла + capability-relay через реальный HTTP.
//!
//! Сценарий (наследие acp-mvp, универсальные участники):
//! 1. заказчик публикует контракт (escrow: бюджет заморожен, Initiated);
//! 2. письмо уходит на relay по capability `ci.compile.rust`;
//! 3. relay fan-out исполнителю, подписанному на `ci.compile.*`;
//! 4. исполнитель принимает, исполняет в песочнице, шлёт доказательство;
//! 5. заказчик проверяет и высвобождает escrow (Released), репутация +1;
//! 6. память обоих узлов — целая цепочка фактов.

use std::sync::{Arc, Mutex};

use axum::Router;
use pactum_contract::{
    Budget, Contract, EscrowEngine, Intent, RoyaltySplit, Settlement, SourceRef, TestCmd,
    Verification,
};
use pactum_core::letter::Letter;
use pactum_identity::KeyPair;
use pactum_node::api::{node_router, NodeHandle};
use pactum_node::orchestrator::{NodeCore, NodeOptions, Role};
use pactum_node::relay::{relay_router, RelayRegistry, RelaySubscription};

fn sample_contract(principal: &str, title: &str) -> Contract {
    Contract {
        schema: "pactum-contract/v1".into(),
        schema_version: "1.0.0".into(),
        contract_id: None, // назначит узел при публикации
        nonce: None,
        intent: Intent {
            kind: "ci.compile.rust".into(),
            domain: "rust".into(),
            title: title.into(),
            source: SourceRef {
                kind: "inline".into(),
                data: Some("console.log('ok')".into()),
                reference: None,
            },
        },
        verification: Verification {
            tests: vec![TestCmd {
                cmd: "printf 'сборка ок'".into(),
                timeout_s: 30,
            }],
            artifacts_expected: vec![],
        },
        settlement: Settlement {
            provider: "sim".into(),
            budget: Budget {
                amount: "0.50".into(),
                unit: "USDC".into(),
            },
            on_success: "release_escrow".into(),
            on_failure: "refund".into(),
            royalty_split: RoyaltySplit {
                principal: 90,
                executor: 10,
            },
            auto_verify: true,
            slash_stake_percent: 10,
        },
        parties: pactum_contract::Parties {
            principal: principal.into(),
            executor: None,
        },
        deadline_utc: None,
        notes: None,
    }
}

async fn spawn_node(core: NodeCore) -> (std::net::SocketAddr, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let inbox_url = format!("http://{addr}/inbox");
    let handle = Arc::new(NodeHandle::new(core, inbox_url));
    let app = node_router(handle);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, format!("http://{addr}/inbox"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_network_lifecycle_over_http() {
    // ---- инфраструктура: relay + два узла с общим escrow-движком ----
    let relay = Arc::new(RelayRegistry::new());
    let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_addr = relay_listener.local_addr().unwrap();
    let relay_app: Router = relay_router(relay.clone());
    tokio::spawn(async move { axum::serve(relay_listener, relay_app).await.unwrap() });

    let engine: Arc<Mutex<EscrowEngine>> = Arc::new(Mutex::new(EscrowEngine::new()));

    let customer_core = NodeCore::with_engine(
        KeyPair::generate(),
        Role::Principal,
        NodeOptions {
            capabilities: vec![],
            ..Default::default()
        },
        engine.clone(),
    );
    let executor_core = NodeCore::with_engine(
        KeyPair::generate(),
        Role::Executor,
        NodeOptions {
            capabilities: vec!["ci.compile.*".into()],
            ..Default::default()
        },
        engine,
    );

    let (customer_addr, _customer_inbox) = spawn_node(customer_core).await;
    let (executor_addr, executor_inbox) = spawn_node(executor_core).await;
    let customer_url = format!("http://{customer_addr}");
    let executor_url = format!("http://{executor_addr}");

    let http = ureq::Agent::new();
    let get_json = |url: &str| -> serde_json::Value {
        serde_json::from_str(&http.get(url).call().unwrap().into_string().unwrap()).unwrap()
    };

    // ---- исполнитель подписывается на capability у релея ----
    let sub = RelaySubscription {
        node_id: "executor".into(),
        capability: "ci.compile.*".into(),
        endpoint: executor_inbox.clone(),
    };
    http.post(&format!("http://{relay_addr}/relay/subscribe"))
        .send_json(serde_json::to_value(sub).unwrap())
        .unwrap();

    // ---- заказчик публикует контракт ----
    let health = get_json(&format!("{customer_url}/health"));
    let principal_id = health["node_id"].as_str().unwrap().to_string();

    let contract = sample_contract(&principal_id, "E2E: собрать wasm");
    let resp: serde_json::Value = serde_json::from_str(
        &http
            .post(&format!("{customer_url}/outbox"))
            .send_string(&serde_json::to_string(&contract).unwrap())
            .unwrap()
            .into_string()
            .unwrap(),
    )
    .unwrap();
    let letter_text = resp["letter"].as_str().unwrap().to_string();
    let capability = resp["capability"].as_str().unwrap().to_string();
    assert_eq!(capability, "ci.compile.rust");

    // escrow заказчика: бюджет заморожен
    let letter = Letter::parse(&letter_text).unwrap();
    let contract_id = letter.body["contract_id"].as_str().unwrap().to_string();
    assert_eq!(
        get_json(&format!("{customer_url}/contracts/{contract_id}"))["escrow_state"],
        "Initiated"
    );

    // ---- диагностический пинг исполнителя напрямую ----
    let ping = http.get(&format!("{executor_url}/../health")).call();
    match ping {
        Ok(r) => println!("executor health: {}", r.status()),
        Err(err) => panic!("сервер исполнителя не отвечает до fan-out: {err}"),
    }

    // ---- письмо уходит в релей по capability ----
    let fanout: serde_json::Value = serde_json::from_str(
        &http
            .post(&format!("http://{relay_addr}/relay/publish"))
            .set("X-Pactum-Capability", &capability)
            .send_string(&letter_text)
            .unwrap()
            .into_string()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        fanout["delivered"],
        serde_json::json!(1),
        "relay доставил исполнителю"
    );

    // ---- исполнитель обрабатывает входящее: accept → execute → submit ----
    let processed: serde_json::Value = serde_json::from_str(
        &http
            .post(&format!("{executor_url}/process"))
            .call()
            .unwrap()
            .into_string()
            .unwrap(),
    )
    .unwrap();
    let outcomes = processed["outcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0]["kind"], "executed");
    assert_eq!(
        outcomes[0]["proof"]["ok"],
        serde_json::json!(true),
        "исполнение прошло"
    );
    // доказательство уже улетело заказчику по reply_to

    // escrow: задача в Completed до проверки
    assert_eq!(
        get_json(&format!("{customer_url}/contracts/{contract_id}"))["escrow_state"],
        "Completed"
    );

    // ---- заказчик проверяет доказательство и высвобождает escrow ----
    let verified: serde_json::Value = serde_json::from_str(
        &http
            .post(&format!("{customer_url}/process"))
            .call()
            .unwrap()
            .into_string()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(verified["outcomes"][0]["kind"], "verified");
    assert_eq!(verified["outcomes"][0]["success"], serde_json::json!(true));
    assert_eq!(
        get_json(&format!("{customer_url}/contracts/{contract_id}"))["escrow_state"],
        "Released"
    );

    // ---- репутация и память ----
    let executor_id = get_json(&format!("{executor_url}/health"))["node_id"]
        .as_str()
        .unwrap()
        .to_string();
    let rep = get_json(&format!("{customer_url}/reputation/{executor_id}"));
    assert_eq!(rep["reputation"], serde_json::json!(1), "успех = +1");

    let customer_facts = get_json(&format!("{customer_url}/facts"));
    assert!(
        customer_facts["facts"].as_array().unwrap().len() >= 3,
        "заказчик помнит сделку"
    );
    let verified_memory = get_json(&format!("{executor_url}/facts/verify"));
    assert_eq!(
        verified_memory["verified"],
        serde_json::json!(true),
        "цепочка фактов цела"
    );

    // ---- релей: очередь для неспешных подписчиков ----
    let hold = http
        .post(&format!("http://{relay_addr}/relay/publish"))
        .set("X-Pactum-Capability", "nobody.subscribed.here")
        .send_string(&letter_text)
        .unwrap()
        .into_string()
        .unwrap();
    assert!(
        hold.contains("queued"),
        "без подписчиков письмо ждёт в очереди"
    );
    let flushed: serde_json::Value = serde_json::from_str(
        &http
            .post(&format!("http://{relay_addr}/relay/flush"))
            .call()
            .unwrap()
            .into_string()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(flushed["flushed"], serde_json::json!(0));
}
