//! `pactum demo` — сквозная демонстрация протокола в одном процессе.
//!
//! Показывает весь жизненный цикл сделки ACP без сети: публикация контракта,
//! escrow, исполнение в песочнице, доказательство, проверка, высвобождение,
//! репутация, память и доказательство-заявление. Всё честно и воспроизводимо.

use pactum_contract::{Contract, EscrowEngine, EscrowState};
use pactum_core::facts::Register;
use pactum_identity::KeyPair;
use pactum_node::orchestrator::{NodeCore, NodeOptions, Outcome, Role};
use std::sync::{Arc, Mutex};

pub fn run() -> i32 {
    println!("Pactum — демонстрация ACP (Autonomous Contract Protocol)");
    println!("участники: заказчик-сервис и исполнитель-агент (роли взаимозаменяемы)");
    println!();

    // ---- участники ----
    let engine = Arc::new(Mutex::new(EscrowEngine::new()));
    let customer = NodeCore::with_engine(
        KeyPair::generate(),
        Role::Principal,
        NodeOptions {
            capabilities: vec![],
            ..Default::default()
        },
        engine.clone(),
    );
    let executor = NodeCore::with_engine(
        KeyPair::generate(),
        Role::Executor,
        NodeOptions {
            capabilities: vec!["ci.compile.*".into()],
            ..Default::default()
        },
        engine,
    );
    println!("[1] участники:");
    println!("    заказчик   {} (Principal)", customer.node_id());
    println!(
        "    исполнитель {} (Executor, умеет ci.compile.*)",
        executor.node_id()
    );

    // ---- контракт ----
    let contract = Contract::build(
        pactum_contract::Intent {
            kind: "ci.compile.rust".into(),
            domain: "rust".into(),
            title: "Демо: проверить сборку".into(),
            source: pactum_contract::SourceRef {
                kind: "inline".into(),
                data: Some("fn main() {}".into()),
                reference: None,
            },
        },
        pactum_contract::Verification {
            tests: vec![pactum_contract::TestCmd {
                cmd: "printf 'сборка ок: 80/80'".into(),
                timeout_s: 30,
            }],
            artifacts_expected: vec![],
        },
        pactum_contract::Settlement {
            provider: "sim".into(),
            budget: pactum_contract::Budget {
                amount: "1.00".into(),
                unit: "USDC".into(),
            },
            on_success: "release_escrow".into(),
            on_failure: "refund".into(),
            royalty_split: pactum_contract::RoyaltySplit {
                principal: 90,
                executor: 10,
            },
            auto_verify: true,
            slash_stake_percent: 10,
        },
        &customer.node_id(),
        None,
    );
    let letter = match customer.publish_contract(contract) {
        Ok(letter) => letter,
        Err(err) => {
            eprintln!("публикация не удалась: {err}");
            return 1;
        }
    };
    let contract_id = letter
        .body
        .get("contract_id")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();
    println!();
    println!("[2] заказчик опубликовал контракт {contract_id}:");
    println!(
        "    capability: {}",
        letter.header("X-Pactum-Capability").unwrap_or("?")
    );
    println!(
        "    бюджет:     {} (заморожен в escrow, Initiated)",
        letter.header("X-Escrow-Budget").unwrap_or("?")
    );
    println!(
        "    подпись:    {}…",
        &letter.header("X-Pactum-Signature").unwrap_or("?")
            [..40.min(letter.header("X-Pactum-Signature").unwrap_or("?").len())]
    );

    // ---- relay-эквивалент: доставка письма исполнителю ----
    println!();
    println!("[3] релей раздал письмо по подписке ci.compile.* (fan-out)");
    executor.receive(letter).expect("письмо принято");

    // ---- исполнение ----
    let outcomes = match executor.process_inbox() {
        Ok(outcomes) => outcomes,
        Err(err) => {
            eprintln!("исполнение не удалось: {err}");
            return 1;
        }
    };
    let (proof_doc, reply_letter) = match &outcomes[0] {
        Outcome::Executed { proof, reply, .. } => (proof.clone(), reply.clone()),
        Outcome::Rejected { reason, .. } => {
            eprintln!("исполнитель отказался: {reason}");
            return 1;
        }
        _ => unreachable!(),
    };
    println!("[4] исполнитель исполнил в песочнице:");
    println!("    exit-коды:  {:?}", proof_doc.exit_codes);
    println!("    вывод:      {}", proof_doc.excerpt);
    println!("    exec_hash:  {}…", &proof_doc.exec_hash[..24]);
    println!("    escrow →    Completed (результат заявлен)");

    // ---- доказательство ----
    println!();
    println!("[5] доказательство улетело заказчику (письмо kind=proof, подписано)");
    customer
        .receive(reply_letter)
        .expect("доказательство принято");

    // ---- проверка и расчёт ----
    let outcomes = match customer.process_inbox() {
        Ok(outcomes) => outcomes,
        Err(err) => {
            eprintln!("проверка не удалась: {err}");
            return 1;
        }
    };
    let success = matches!(&outcomes[0], Outcome::Verified { success: true, .. });
    let state = customer
        .escrow_state(&contract_id)
        .unwrap_or(EscrowState::Initiated);
    println!(
        "[6] заказчик проверил доказательство: success={success}, escrow → {}",
        state.as_str()
    );
    println!("    делёж: 90% principal / 10% executor, репутация исполнителя +1");

    // ---- память ----
    println!();
    println!("[7] память узлов (hash chain, подделка ломает цепочку):");
    println!(
        "    заказчик:   {} фактов",
        customer.facts.lock().map(|c| c.facts.len()).unwrap_or(0)
    );
    println!(
        "    исполнитель: {} фактов",
        executor.facts.lock().map(|c| c.facts.len()).unwrap_or(0)
    );
    println!(
        "    верификация: заказчик {:?}, исполнитель {:?}",
        customer.verify_memory().is_ok(),
        executor.verify_memory().is_ok(),
    );

    // ---- proof-заявление ----
    println!();
    println!("[8] бонус: заявление с доказательством (примитив proof)");
    let dir = std::env::temp_dir().join(format!("pactum-demo-{}", pactum_core::id::random_hex(4)));
    let _ = std::fs::create_dir_all(&dir);
    let ledger = match pactum_proof::Ledger::open(dir.join("proofs")) {
        Ok(ledger) => ledger,
        Err(err) => {
            eprintln!("реестр не открылся: {err}");
            return 1;
        }
    };
    let mut claim = pactum_proof::Claim::new(
        ledger.next_id(),
        "демо-вывод стабилен",
        "printf 'Ran 80 tests in 0.01s\\nOK'",
        "",
        "demo",
        0,
        10,
        pactum_core::time::now(),
    );
    match pactum_proof::check_now(&mut claim) {
        Ok(status) => {
            let _ = ledger.put(&claim);
            println!(
                "    #{} {} «{}» → отпечаток {}",
                claim.id,
                status.ru(),
                claim.claim,
                claim.digest
            );
        }
        Err(err) => {
            eprintln!("    заявление не проверено: {err}");
            return 1;
        }
    }
    let _ = std::fs::remove_dir_all(&dir);

    // ---- итог ----
    let rep = customer.reputation(&executor.node_id());
    println!();
    println!(
        "итог: контракт {contract_id} → {}, репутация исполнителя {rep:+}",
        state.as_str()
    );
    let facts_ok = customer.verify_memory().is_ok() && executor.verify_memory().is_ok();
    let demo_record = customer
        .facts
        .lock()
        .map(|chain| {
            chain
                .filter(Some(Register::Fact), Some(contract_id.as_str()))
                .len()
        })
        .unwrap_or(0);
    if facts_ok && state == EscrowState::Released && rep == 1 && demo_record > 0 {
        println!("статус: демо чистое — сделка доказана, память цела");
        0
    } else {
        eprintln!("статус: демо провалило проверки");
        1
    }
}
