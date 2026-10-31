//! `pactum facts` — цепочка фактов узла.

use clap::Subcommand;
use pactum_core::facts::{FactChain, Register};
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum FactsCommand {
    /// Записать факт (хеш-цепочка продолжится сама)
    Add {
        /// Файл памяти
        #[arg(long, default_value = "data/facts.json")]
        store: PathBuf,
        /// Регистр: SENSE | FACT | LOGIC | OPINION | ACTION
        #[arg(long)]
        register: String,
        /// Текст факта
        #[arg(long)]
        text: String,
        #[arg(long)]
        contract: Option<String>,
    },
    /// Проверить всю цепочку
    Verify {
        #[arg(long, default_value = "data/facts.json")]
        store: PathBuf,
    },
    /// Показать факты (с фильтрами)
    List {
        #[arg(long, default_value = "data/facts.json")]
        store: PathBuf,
        #[arg(long)]
        register: Option<String>,
        #[arg(long)]
        contract: Option<String>,
    },
}

pub fn run(command: FactsCommand) -> i32 {
    match command {
        FactsCommand::Add {
            store,
            register,
            text,
            contract,
        } => {
            let register = match Register::parse(&register) {
                Some(register) => register,
                None => {
                    eprintln!("регистр не распознан: SENSE | FACT | LOGIC | OPINION | ACTION");
                    return 2;
                }
            };
            let mut chain = load_chain(&store);
            let (seq, hash) = {
                let fact = chain.append(register, &text, contract.as_deref());
                (fact.seq, fact.hash.clone())
            };
            if let Err(err) = chain.save(&store) {
                eprintln!("память не записана: {err}");
                return 1;
            }
            println!("факт #{seq} записан: {hash}");
            0
        }
        FactsCommand::Verify { store } => {
            if !store.exists() {
                println!("памяти нет — проверять нечего");
                return 0;
            }
            match FactChain::load(&store) {
                Ok(chain) => {
                    println!(
                        "цепочка цела: {} фактов, последний {}",
                        chain.facts.len(),
                        chain.facts.last().map(|f| f.hash.as_str()).unwrap_or("-")
                    );
                    0
                }
                Err(err) => {
                    eprintln!("цепочка повреждена: {err}");
                    1
                }
            }
        }
        FactsCommand::List {
            store,
            register,
            contract,
        } => {
            let chain = load_chain(&store);
            let register = match register.as_deref() {
                Some(text) => match Register::parse(text) {
                    Some(parsed) => Some(parsed),
                    None => {
                        eprintln!("регистр не распознан: SENSE | FACT | LOGIC | OPINION | ACTION");
                        return 2;
                    }
                },
                None => None,
            };
            let filtered = chain.filter(register, contract.as_deref());
            if filtered.is_empty() {
                println!("фактов нет");
                return 0;
            }
            for fact in filtered {
                println!(
                    "#{}  {}  {}  {}",
                    fact.seq,
                    fact.register.as_str(),
                    fact.ts,
                    fact.text
                );
            }
            0
        }
    }
}

fn load_chain(store: &Path) -> FactChain {
    if store.exists() {
        FactChain::load(store).unwrap_or_default()
    } else {
        FactChain::new()
    }
}
