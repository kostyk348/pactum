//! `pactum` — один бинарник для всего протокола ACP:
//! контракты, хаб подписок, сканер источников, доказательства, память, узлы
//! и нотариус контента.
//!
//! Денежная часть (escrow) — под feature-флагом `escrow` (включён по
//! умолчанию): `cargo build --no-default-features` даёт бинарник без
//! контрактов и узлов — хаб, сканер, proof, факты, witness и id остаются.

#[cfg(feature = "escrow")]
mod cmd_contract;
#[cfg(feature = "escrow")]
mod cmd_demo;
mod cmd_facts;
mod cmd_hub;
mod cmd_id;
#[cfg(feature = "escrow")]
mod cmd_node;
mod cmd_proof;
mod cmd_scan;
mod cmd_witness;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "pactum",
    version,
    about = "Pactum — референс-реализация ACP (Autonomous Contract Protocol)",
    long_about = "Pactum — контракты с escrow (опционально), честный хаб подписок,\nперепроверяемые доказательства и нотариус контента.\nУчастник протокола — что угодно: человек, сервис, устройство или агент."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Идентичность узла: ключи, keystore, node id
    #[command(subcommand)]
    Id(cmd_id::IdCommand),
    /// Хаб подписок: сервер, подписки, публикации, журнал
    #[command(subcommand)]
    Hub(cmd_hub::HubCommand),
    /// Сканер источников: наблюдение, кандидаты, публикация изменений
    #[command(subcommand)]
    Scan(cmd_scan::ScanCommand),
    /// Доказательства: заявления с командой и отпечатком вывода
    #[command(subcommand)]
    Proof(cmd_proof::ProofCommand),
    /// Память узла: цепочка фактов, которую нельзя подделать
    #[command(subcommand)]
    Facts(cmd_facts::FactsCommand),
    /// Нотариус контента: подписанные приёмники и журнал-цепочка
    #[command(subcommand)]
    Witness(cmd_witness::WitnessCommand),
    /// Контракты: создание, проверка, жизненный цикл сделки (feature: escrow)
    #[cfg(feature = "escrow")]
    #[command(subcommand)]
    Contract(cmd_contract::ContractCommand),
    /// Узел сети: REST API + хаб + relay в одном процессе (feature: escrow)
    #[cfg(feature = "escrow")]
    #[command(subcommand)]
    Node(cmd_node::NodeCommand),
    /// Сквозная демонстрация протокола в одном процессе (feature: escrow)
    #[cfg(feature = "escrow")]
    Demo,
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Id(cmd) => cmd_id::run(cmd),
        Command::Hub(cmd) => cmd_hub::run(cmd),
        Command::Scan(cmd) => cmd_scan::run(cmd),
        Command::Proof(cmd) => cmd_proof::run(cmd),
        Command::Facts(cmd) => cmd_facts::run(cmd),
        Command::Witness(cmd) => cmd_witness::run(cmd),
        #[cfg(feature = "escrow")]
        Command::Contract(cmd) => cmd_contract::run(cmd),
        #[cfg(feature = "escrow")]
        Command::Node(cmd) => cmd_node::run(cmd),
        #[cfg(feature = "escrow")]
        Command::Demo => cmd_demo::run(),
    };
    std::process::exit(code);
}
