//! `pactum contract` — создание и проверка контрактов.

use clap::Subcommand;
use pactum_contract::{
    Budget, Contract, Intent, RoyaltySplit, Settlement, SourceRef, TestCmd, Verification,
};
use std::path::PathBuf;

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub enum ContractCommand {
    /// Собрать контракт из параметров и сохранить в JSON
    New {
        /// Capability задачи: ci.compile.rust, deploy.site, data.backup…
        #[arg(long)]
        capability: String,
        /// Заголовок работы
        #[arg(long)]
        title: String,
        /// Проверочная команда (можно повторять --cmd несколько раз)
        #[arg(long = "cmd")]
        cmds: Vec<String>,
        /// Домен/область
        #[arg(long, default_value = "")]
        domain: String,
        /// Источник: inline данные
        #[arg(long)]
        inline: Option<String>,
        /// Источник: git-ссылка
        #[arg(long)]
        git: Option<String>,
        /// Источник: url
        #[arg(long)]
        url: Option<String>,
        /// Источник: файл
        #[arg(long)]
        file: Option<String>,
        /// Бюджет (строка без float-магии: "1.00")
        #[arg(long, default_value = "0.01")]
        budget: String,
        /// Единица бюджета: USDC, USD, кредиты…
        #[arg(long, default_value = "USDC")]
        unit: String,
        /// Доля исполнителя, % (остальное — заказчику)
        #[arg(long, default_value_t = 10)]
        royalty: i64,
        /// Дедлайн ISO-8601 UTC, например 2026-12-31T23:59:59Z
        #[arg(long)]
        deadline: Option<String>,
        /// Ожидаемые артефакты: путь[:мин_размер] (можно несколько)
        #[arg(long = "artifact")]
        artifacts: Vec<String>,
        /// Куда сохранить контракт
        #[arg(long, default_value = "contract.json")]
        out: PathBuf,
    },
    /// Проверить контракт по схеме и показать сводку
    Validate {
        #[arg(long, default_value = "contract.json")]
        file: PathBuf,
    },
    /// Сухой прогон проверок контракта в папке (без escrow)
    DryRun {
        #[arg(long, default_value = "contract.json")]
        file: PathBuf,
        /// Рабочая папка с исходниками
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
}

pub fn run(command: ContractCommand) -> i32 {
    match command {
        ContractCommand::New {
            capability,
            title,
            cmds,
            domain,
            inline,
            git,
            url,
            file,
            budget,
            unit,
            royalty,
            deadline,
            artifacts,
            out,
        } => {
            if cmds.is_empty() {
                eprintln!("нужна хотя бы одна проверочная команда: --cmd");
                return 2;
            }
            let source = if let Some(data) = inline {
                SourceRef {
                    kind: "inline".into(),
                    data: Some(data),
                    reference: None,
                }
            } else if let Some(git_ref) = git {
                SourceRef {
                    kind: "git".into(),
                    data: None,
                    reference: Some(git_ref),
                }
            } else if let Some(url_ref) = url {
                SourceRef {
                    kind: "url".into(),
                    data: None,
                    reference: Some(url_ref),
                }
            } else if let Some(file_ref) = file {
                SourceRef {
                    kind: "file".into(),
                    data: None,
                    reference: Some(file_ref),
                }
            } else {
                eprintln!("нужен источник: --inline | --git | --url | --file");
                return 2;
            };
            let executor_share = royalty;
            let contract = Contract::build(
                Intent {
                    kind: capability,
                    domain,
                    title,
                    source,
                },
                Verification {
                    tests: cmds
                        .into_iter()
                        .map(|cmd| TestCmd {
                            cmd,
                            timeout_s: 120,
                        })
                        .collect(),
                    artifacts_expected: artifacts
                        .into_iter()
                        .map(|raw| {
                            let (path, size) = raw.split_once(':').unwrap_or((raw.as_str(), "0"));
                            pactum_contract::ArtifactExpect {
                                path: path.to_string(),
                                min_size: size.parse().unwrap_or(0),
                            }
                        })
                        .collect(),
                },
                Settlement {
                    provider: "sim".into(),
                    budget: Budget {
                        amount: budget,
                        unit,
                    },
                    on_success: "release_escrow".into(),
                    on_failure: "refund".into(),
                    royalty_split: RoyaltySplit {
                        principal: 100 - executor_share,
                        executor: executor_share,
                    },
                    auto_verify: true,
                    slash_stake_percent: 10,
                },
                // principal назначит узел при публикации; здесь — плейсхолдер
                "p0000000000000000",
                deadline,
            );
            match contract.validate() {
                Ok(()) => {
                    let text = serde_json::to_string_pretty(&contract).expect("сериализация");
                    if let Some(parent) = out.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if std::fs::write(&out, text).is_err() {
                        eprintln!("не записать {}", out.display());
                        return 1;
                    }
                    println!("контракт сохранён: {}", out.display());
                    println!("capability: {}", contract.capability());
                    println!("бюджет: {}", contract.budget_display());
                    println!("id: {}", contract.contract_id.as_deref().unwrap_or("?"));
                    0
                }
                Err(err) => {
                    eprintln!("контракт не валиден: {err}");
                    1
                }
            }
        }
        ContractCommand::Validate { file } => match std::fs::read_to_string(&file) {
            Ok(text) => match Contract::from_json(&text) {
                Ok(contract) => {
                    println!("контракт валиден");
                    println!("capability: {}", contract.capability());
                    println!("бюджет: {}", contract.budget_display());
                    println!("проверок: {}", contract.verification.tests.len());
                    println!(
                        "артефактов: {}",
                        contract.verification.artifacts_expected.len()
                    );
                    0
                }
                Err(err) => {
                    eprintln!("контракт не валиден: {err}");
                    1
                }
            },
            Err(err) => {
                eprintln!("файл не прочитан: {err}");
                1
            }
        },
        ContractCommand::DryRun { file, dir } => match std::fs::read_to_string(&file) {
            Ok(text) => match Contract::from_json(&text) {
                Ok(contract) => match pactum_node::sandbox::dry_run(&dir, &contract) {
                    Ok(proof) => {
                        println!("ok: {}", proof.ok);
                        println!("exit-коды: {:?}", proof.exit_codes);
                        println!("exec_hash: {}", proof.exec_hash);
                        println!("длительность: {} мс", proof.duration_ms);
                        if !proof.excerpt.is_empty() {
                            println!("вывод (хвост):");
                            for line in proof.excerpt.lines() {
                                println!("  {line}");
                            }
                        }
                        if proof.ok {
                            0
                        } else {
                            1
                        }
                    }
                    Err(err) => {
                        eprintln!("сухой прогон провалился: {err}");
                        1
                    }
                },
                Err(err) => {
                    eprintln!("контракт не валиден: {err}");
                    1
                }
            },
            Err(err) => {
                eprintln!("файл не прочитан: {err}");
                1
            }
        },
    }
}
