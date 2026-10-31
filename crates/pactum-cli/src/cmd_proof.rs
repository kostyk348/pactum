//! `pactum proof` — заявления с доказательствами.

use clap::Subcommand;
use pactum_proof::{check_now, Ledger, Status};
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum ProofCommand {
    /// Заявить и сразу проверить: заявление не сохраняется непроверенным
    Add {
        /// Что утверждается: «80 тестов зелёные»
        #[arg(long)]
        claim: String,
        /// Чем проверяется: точная команда
        #[arg(long)]
        cmd: String,
        /// Где запускать
        #[arg(long, default_value = ".")]
        dir: String,
        /// Кто заявил
        #[arg(long, default_value = "")]
        by: String,
        /// Ожидаемый код возврата («проверка должна падать» — тоже доказательство)
        #[arg(long, default_value_t = 0)]
        expect: i32,
        /// Тайм-аут команды, секунд
        #[arg(long, default_value_t = pactum_proof::DEFAULT_TIMEOUT_S)]
        timeout: u64,
        /// Каталог реестра
        #[arg(long, default_value = "proofs")]
        ledger: PathBuf,
    },
    /// Перепроверить всё (или одно) — код 1, если есть сломанное
    Check {
        #[arg(long)]
        id: Option<u32>,
        #[arg(long, default_value = "proofs")]
        ledger: PathBuf,
    },
    /// Список заявлений
    List {
        #[arg(long, default_value = "proofs")]
        ledger: PathBuf,
    },
    /// Показать заявление целиком
    Show {
        #[arg(long)]
        id: u32,
        #[arg(long, default_value = "proofs")]
        ledger: PathBuf,
    },
}

pub fn run(command: ProofCommand) -> i32 {
    match command {
        ProofCommand::Add {
            claim,
            cmd,
            dir,
            by,
            expect,
            timeout,
            ledger,
        } => {
            let ledger = match Ledger::open(&ledger) {
                Ok(ledger) => ledger,
                Err(err) => {
                    eprintln!("реестр не открылся: {err}");
                    return 2;
                }
            };
            let id = ledger.next_id();
            let mut entry = pactum_proof::Claim::new(
                id,
                &claim,
                &cmd,
                &dir,
                &by,
                expect,
                timeout,
                pactum_core::time::now(),
            );
            match check_now(&mut entry) {
                Ok(status) => {
                    if ledger.put(&entry).is_err() {
                        eprintln!("заявление не записано");
                        return 1;
                    }
                    println!("#{id}  {}  {claim}", status_ru(status));
                    println!("    команда: {cmd}");
                    println!("    отпечаток: {}", entry.digest);
                    0
                }
                Err(err) => {
                    eprintln!("заявление не проверено: {err}");
                    1
                }
            }
        }
        ProofCommand::Check { id, ledger } => {
            let ledger = match Ledger::open(&ledger) {
                Ok(ledger) => ledger,
                Err(err) => {
                    eprintln!("реестр не открылся: {err}");
                    return 2;
                }
            };
            let targets: Vec<pactum_proof::Claim> = match id {
                Some(id) => vec![match ledger.get(id) {
                    Some(claim) => claim,
                    None => {
                        eprintln!("заявление #{id} не найдено");
                        return 1;
                    }
                }],
                None => ledger.claims(),
            };
            if targets.is_empty() {
                println!("заявлений нет");
                return 0;
            }
            let mut verified = 0u32;
            let mut moved = 0u32;
            let mut broken = 0u32;
            let mut failed = false;
            for mut claim in targets {
                match check_now(&mut claim) {
                    Ok(status) => {
                        println!("#{}  {}  {}", claim.id, status_ru(status), claim.claim);
                        match status {
                            Status::Verified => verified += 1,
                            Status::Moved => moved += 1,
                            Status::Broken => broken += 1,
                            Status::Never => {}
                        }
                        if ledger.put(&claim).is_err() {
                            eprintln!("заявление #{} не записано", claim.id);
                            failed = true;
                        }
                    }
                    Err(err) => {
                        println!("#{}  ошибка  {}  ({err})", claim.id, claim.claim);
                        broken += 1;
                    }
                }
            }
            println!();
            println!("итог: подтверждено {verified}, устарело {moved}, сломано {broken}");
            if broken > 0 {
                1
            } else if failed {
                2
            } else {
                0
            }
        }
        ProofCommand::List { ledger } => {
            let ledger = match Ledger::open(&ledger) {
                Ok(ledger) => ledger,
                Err(err) => {
                    eprintln!("реестр не открылся: {err}");
                    return 2;
                }
            };
            let claims = ledger.claims();
            if claims.is_empty() {
                println!("заявлений нет");
                return 0;
            }
            for claim in &claims {
                println!(
                    "#{}  {}  {}  [{}]",
                    claim.id,
                    status_ru(claim.status),
                    claim.claim,
                    claim.cmd
                );
            }
            0
        }
        ProofCommand::Show { id, ledger } => {
            let ledger = match Ledger::open(&ledger) {
                Ok(ledger) => ledger,
                Err(err) => {
                    eprintln!("реестр не открылся: {err}");
                    return 2;
                }
            };
            match ledger.get(id) {
                Some(claim) => {
                    println!("{}", claim.render());
                    0
                }
                None => {
                    eprintln!("заявление #{id} не найдено");
                    1
                }
            }
        }
    }
}

fn status_ru(status: Status) -> &'static str {
    status.ru()
}
