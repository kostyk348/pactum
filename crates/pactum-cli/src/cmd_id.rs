//! `pactum id` — идентичность узла.

use clap::Subcommand;
use pactum_identity::{load_keystore, save_keystore, KeyPair};
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum IdCommand {
    /// Сгенерировать новую пару ключей и сохранить в keystore
    New {
        /// Файл keystore
        #[arg(long, default_value = "pactum-key.json")]
        keystore: PathBuf,
        /// Парольная фраза для шифрования (иначе ключ в открытом виде, chmod 600)
        #[arg(long, env = "PACTUM_PASSPHRASE")]
        passphrase: Option<String>,
    },
    /// Показать node id и открытый ключ из keystore
    Show {
        #[arg(long, default_value = "pactum-key.json")]
        keystore: PathBuf,
        #[arg(long, env = "PACTUM_PASSPHRASE")]
        passphrase: Option<String>,
    },
}

pub fn run(command: IdCommand) -> i32 {
    match command {
        IdCommand::New {
            keystore,
            passphrase,
        } => {
            let key = KeyPair::generate();
            match save_keystore(&keystore, &key, passphrase.as_deref()) {
                Ok(()) => {
                    if passphrase.is_none() {
                        eprintln!("внимание: ключ сохранён без шифрования — задайте --passphrase для AES-GCM");
                    }
                    println!("node id: {}", key.node_id());
                    println!("pubkey:  {}", key.pubkey_hex());
                    println!("keystore: {}", keystore.display());
                    0
                }
                Err(err) => {
                    eprintln!("keystore не сохранён: {err}");
                    1
                }
            }
        }
        IdCommand::Show {
            keystore,
            passphrase,
        } => match load_keystore(&keystore, passphrase.as_deref()) {
            Ok(key) => {
                println!("node id: {}", key.node_id());
                println!("pubkey:  {}", key.pubkey_hex());
                0
            }
            Err(err) => {
                eprintln!("keystore не прочитан: {err}");
                1
            }
        },
    }
}
