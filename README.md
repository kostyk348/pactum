<div align="right"><b>English</b> · <a href="README.ru.md">Русский</a></div>

# Pactum

[![Release](https://img.shields.io/github/v/release/kostyk348/pactum?sort=semver&color=blue)](https://github.com/kostyk348/pactum/releases)
[![CI](https://img.shields.io/github/actions/workflow/status/kostyk348/pactum/ci.yml?branch=main&label=CI)](https://github.com/kostyk348/pactum/actions)
[![License](https://img.shields.io/badge/license-Apache--2.0-green)](LICENSE)
[![Protocol](https://img.shields.io/badge/protocol-acp%2F1-blueviolet)](docs/PROTOCOL.md)

🌐 [kostyk348.github.io/pactum](https://kostyk348.github.io/pactum/) — bilingual landing page (EN/RU).
🖥️ [Pactum Console](https://kostyk348.github.io/pactum/console/) — live in-browser demo: three agents (Scout → Digest → Auditor) run a real ACP/1 scenario — signed letters, hub challenge + honest refusals, a live SHA-256 fact chain, proof verification, reputation. Budget 0.00 USDC: no money anywhere.

**Infrastructure for honest exchange between any autonomous participants — humans, services, devices, and agents alike.**

In plain words: ACP (Autonomous Contract Protocol) is a way for independent parties to make commitments to each other, prove that commitments are fulfilled, and settle them — without a middleman and without taking anyone's word for it. One party publishes a machine-readable **contract**, another fulfills it in a **sandbox** and attaches **proof**, the budget sits in **escrow** until the proof checks out, and a **subscription hub** distributes events with honest refusals instead of silent loss.

Everything travels as plain **letters** — RFC-822-style envelopes with a JSON body. No magic: a letter can be read in any editor, kept in git, and sent by anything.

Pactum is the reference implementation: a Rust workspace, one binary, production stack (tokio + axum + clap + serde).

---

## Contents

1. [Why universal](#why-universal)
2. [Three primitives](#three-primitives)
3. [Quick start](#quick-start)
4. [How a deal works](#how-a-deal-works)
5. [Who is a participant](#who-is-a-participant)
6. [Crates](#crates)
7. [The CLI at a glance](#the-cli-at-a-glance)
8. [Settlement and the Solidity reference](#settlement-and-the-solidity-reference)
9. [Security](#security)
10. [Honest limitations](#honest-limitations)
11. [Roadmap](#roadmap)
12. [Lineage and rebranding](#lineage-and-rebranding)
13. [FAQ](#faq)
14. [License](#license)

---

## Why universal

The protocol grew out of [acp-mvp](https://github.com/kostyk348/acp-mvp) — a network where AI agents contracted each other for work. The concept turned out bigger than agents. The same three questions define honest exchange for *any* autonomous parties:

- how do we **agree** — a contract, not a chat message;
- how do we **prove** it happened — a re-verifiable proof, not "trust me";
- how do we **spread the word** — subscriptions with honest refusals, not silent polling.

So the "agent" went out of the name. **ACP is now Autonomous Contract Protocol**, and a participant is anything that can hold a key and act: a person, a cron job, a microservice, a PLC, a team, an agent.

## Three primitives

| Primitive | Lineage | What it is |
|---|---|---|
| **Contract** | [acp-mvp](https://github.com/kostyk348/acp-mvp) | obligation + verification rules + escrow + reputation; lifecycle `Initiated → Accepted → Completed → Released / Refunded / Slashed` |
| **Hub** | [hubd](https://github.com/kostyk348/hubd) | topic subscriptions with leases, persisted retry queue, honest refusal (`404` = "not for me", `hub.mode=denied`), `413` on oversize — nothing is ever silently lost |
| **Proof** | [proof](https://github.com/kostyk348/proof) | a claim + the exact command + an output fingerprint; three verdicts (`verified / moved / broken`); anyone re-checks it with one command |

Plus a shared foundation: **letters** (signed RFC-822 envelopes), a **capability relay** (fan-out by skill, wildcards allowed), and a **fact chain** — hash-linked memory that breaks if anyone tampers with it.

## Quick start

```bash
git clone https://github.com/kostyk348/pactum && cd pactum
cargo build --release          # one binary: target/release/pactum
cargo test --workspace         # 90+ tests, incl. a full network e2e
```

End-to-end demo of the whole protocol in one process:

```bash
pactum demo
```

```
Pactum — демонстрация ACP (Autonomous Contract Protocol)
[1] участники: заказчик pc85e…, исполнитель p4ea3…
[2] контракт c-cd4a… опубликован, бюджет 1.00 USDC заморожен (Initiated)
[3] релей раздал письмо по подписке ci.compile.*
[4] исполнитель исполнил: exit [0], «сборка ок: 80/80», escrow → Completed
[6] доказательство проверено, escrow → Released, репутация +1
[7] память обоих узлов — целая hash chain
итог: контракт → Released, репутация исполнителя +1
```

No Rust toolchain at hand? Open [`docs/console/index.html`](docs/console/index.html) — the same protocol, live in your browser, zero install.

<details>
<summary><b>Network walkthrough: two nodes + relay</b></summary>

```bash
# terminal 1 — relay
pactum hub serve --port 9001 --store data/hub.json

# terminal 1 — customer node
pactum node serve --port 9002 --role principal --keystore keys/customer.json

# terminal 2 — executor node
pactum node serve --port 9003 --role executor \
    --capabilities 'ci.compile.*,deploy.*' --keystore keys/executor.json

# subscribe the executor at the relay
curl -s http://127.0.0.1:9001/relay/subscribe -H 'Content-Type: application/json' \
    -d '{"node_id":"executor","capability":"ci.compile.*","endpoint":"http://127.0.0.1:9003/inbox"}'

# publish a contract from the customer
pactum contract new --capability ci.compile.rust --title "Build wasm" \
    --cmd 'cargo build --release' --git https://github.com/example/repo \
    --budget 1.00 --unit USDC --out contract.json
curl -s http://127.0.0.1:9002/outbox --data-binary @contract.json

# executor processes its inbox (accept → sandbox → proof letter back)
curl -s -X POST http://127.0.0.1:9003/process
# customer verifies the proof and releases escrow
curl -s -X POST http://127.0.0.1:9002/process
```

Full script with comments: [`examples/two-nodes.sh`](examples/two-nodes.sh).
</details>

<details>
<summary><b>Hub, scanner and proofs — the everyday commands</b></summary>

```bash
# hub: serve / subscribe / publish / journal
pactum hub serve --port 8765 --store data/hub.json
pactum hub listen --port 8766                       # subscriber helper: echoes challenges
pactum hub subscribe --hub http://127.0.0.1:8765 \
    --topic news.rust --callback http://127.0.0.1:8766/
pactum hub publish --hub http://127.0.0.1:8765 --topic news.rust --text "hello"

# scanner: watch any source — url, file, or command output
cp watchlist.example.json watchlist.json
pactum scan run --watchlist watchlist.json --state data/scan.json --queue data/candidates.jsonl
pactum scan candidates --file data/candidates.jsonl --status new --show-sample
pactum scan accept --file data/candidates.jsonl cand-abc123
pactum scan run --watchlist watchlist.json --state data/scan.json \
    --queue data/candidates.jsonl --publish --hub http://127.0.0.1:8765

# proofs: claims anyone can re-verify (exit 1 on CI if broken)
pactum proof add --claim "80 tests green" --cmd "python3 -m unittest discover" --by "ci"
pactum proof check
pactum proof list

# memory: tamper-evident fact chain
pactum facts add --register FACT --text "release 1.0 deployed" --contract c-0c0ffee
pactum facts verify
```
</details>

## How a deal works

```
        PARTICIPANTS (human / service / device / agent)

┌───────────────┐   contract letter  ┌──────────────────┐  fan-out   ┌──────────────────┐
│  Principal    │ ─────────────────▶ │  RELAY           │ ─────────▶ │  Executor        │
│  (customer)   │                    │  by capability:  │            │  (any kind)      │
│               │                    │  who can do X?   │            │                  │
│ 1. publishes  │                    └──────────────────┘            │ 1. accept        │
│ 2. locks      │                                                    │ 2. run sandbox   │
│    budget in  │              ┌──────────────────────┐              │ 3. attach proof  │
│    escrow     │   proof letter│  ESCROW (sim or     │              │    (exec_hash,   │
│               │ ◀──────────── │  AgentEscrow.sol)   │              │    exit codes)   │
│ 4. verifies   │               │  - holds the budget │              └──────────────────┘
│ 5. releases   │               │  - releases / slashes│
└───────────────┘               └──────────────────────┘

Reputation lives with settlement: success +1, failure −1, silence past deadline −5.
```

- the budget cannot be withdrawn before proof — nobody can touch it;
- the executor's stake is slashed if they go silent past the deadline;
- facts about the deal are written to a hash chain on both nodes.

## Who is a participant

| Participant | As principal | As executor | As subscriber |
|---|---|---|---|
| Human / team | orders a task, verifies proofs | does the work, proves it | gets notifications to a listener |
| Service / cron | orders deploys, backups | runs them, proves with `proof` | watches topics for incidents |
| Device / IoT | — | reports data as events | receives configuration updates |
| AI agent | orders subtasks | executes tasks in its sandbox | monitors its skills' demand |

The CLI is made for humans, the REST API (`/inbox`, `/outbox`, `/process`, `/hub/*`, `/relay/*`) for software. Same protocol, same letters.

## Crates

| Crate | What's inside |
|---|---|
| [`pactum-core`](crates/pactum-core) | ids, SHA-256, letters, JSON-schema validator, fact chain |
| [`pactum-identity`](crates/pactum-identity) | ECDSA P-256 keys, encrypted keystore (AES-GCM + PBKDF2) |
| [`pactum-contract`](crates/pactum-contract) | contract model + schema, escrow engine, `SettlementProvider`, reputation |
| [`pactum-hub`](crates/pactum-hub) | topics, leases, retry queue, honest refusals, HTTP API |
| [`pactum-scan`](crates/pactum-scan) | url/file/cmd sources, conditional GET, dedup, candidate queue |
| [`pactum-proof`](crates/pactum-proof) | claims, normalization, verdicts, flat-file ledger |
| [`pactum-node`](crates/pactum-node) | orchestrator, sandbox, REST API, capability relay |
| [`pactum-cli`](crates/pactum-cli) | the `pactum` binary: everything above in one CLI |

Protocol specification: [`docs/PROTOCOL.md`](docs/PROTOCOL.md) — letters, contract schema, hub protocol, proof format, security model.

## The CLI at a glance

| Command group | What it does |
|---|---|
| `pactum id new\|show` | keypair, encrypted keystore, node id |
| `pactum contract new\|validate\|dry-run` | build and check contracts, dry-run verification |
| `pactum hub serve\|subscribe\|publish\|journal\|health\|listen` | the honest pubsub hub |
| `pactum scan run\|candidates\|accept\|reject` | watch sources, review candidates |
| `pactum proof add\|check\|list\|show` | re-verifiable claims (CI-friendly exit codes) |
| `pactum facts add\|verify\|list` | hash-chain memory |
| `pactum node serve` | full node: API + hub + relay + maintenance ticks |
| `pactum demo` | the whole protocol in one process |

## Settlement and the Solidity reference

Settlement is a trait (`SettlementProvider`), not a belief system:

- **`sim`** — built-in deterministic escrow engine: the full lifecycle, royalty split, slashing, reputation; works locally, no chain needed;
- **blockchain** — [`contracts/hardhat/contracts/AgentEscrow.sol`](contracts/README.md) from acp-mvp is the reference backend (ERC-20 + native ETH, `slashOnTimeout`, on-chain reputation), with 10 hardhat tests. A web3 adapter is on the roadmap; the trait is already stable.

### Money is optional

Nothing above requires a payment rail. The escrow engine stores the budget as an opaque string — `"0"` is a valid budget and the lifecycle (frozen → released / refunded) behaves identically — so reputation (+1 / −1 / −5) remains the enforcement. Hub, scanner, proofs and fact chains don't touch settlement at all: each is a standalone crate (see [Crates](#crates)). Typical no-money setups:

- the in-browser [console demo](https://kostyk348.github.io/pactum/console/) — subscribe → contract → collect → deliver → proof → release, the whole happy path on a zero budget (and you can inject a 413, an honest 404, or a tampered fact to watch reputation answer);
- services exchanging signed events via the hub (`pactum hub serve`) — with retries and honest refusals instead of silent drops;
- claims checked in CI via `pactum proof` — verdicts verified / moved / broken, 120 s command timeout;
- audit trails kept as fact chains and verified with `GET /facts/verify`;
- zero-budget contracts, where the only currency is reputation.

## Security

| Threat | Mitigation |
|---|---|
| Forged proof | exec_hash = SHA256(stdout+stderr), exit codes, artifact checks, rule-bound auto-verify |
| Executor vanishes | deadline + `slashOnTimeout` + reputation −5 |
| Budget stolen | escrow holds funds until proof; release only by contract rules |
| Spam | minimum budget, capability subscriptions, reputation |
| Tampered history | hash chain on every node |
| Forged letters | ECDSA P-256 over canonical bytes; keystore AES-256-GCM + PBKDF2 (200k) |

## Honest limitations

- **Sandbox ≠ TEE.** v1.0 executes verification locally with timeouts; for hostile code, plug a docker/TEE backend into the `Sandbox` trait (the seam is ready).
- **No TLS in the relay itself** — run behind TLS or on a trusted network.
- **Scanner speaks http** for URL sources (https through the rustls-backed client is wired in; conditional GET applies to both).
- **SMTP transport is on the roadmap** — letters are already RFC-822-shaped; HTTP relay and file spool are built-in.
- **A single hub is a single process** — its state is a JSON file; clustering is a roadmap item.

## Roadmap

1. web3 adapter for `AgentEscrow.sol` (Sepolia first);
2. TLS for relay and node-to-node letters;
3. SMTP transport adapter (letters are already envelope-shaped);
4. docker/TEE sandbox backends;
5. node discovery (DNS registry / DHT instead of manual subscriptions);
6. bidirectional stake (principals post deposits against "ordered and vanished");
7. hub clustering and replication.

## Lineage and rebranding

Pactum unifies three repositories of the same author, renamed and generalized from "agents only" to any autonomous participants:

| Was | Became |
|---|---|
| [acp-mvp](https://github.com/kostyk348/acp-mvp) — Agent Contract Protocol (Python + Solidity) | **Contract** primitive: `pactum-contract`, `pactum-node`, `contracts/` (Rust + the same Solidity reference) |
| [hubd](https://github.com/kostyk348/hubd) — subscription hub & scanner (Python) | **Hub** primitive: `pactum-hub`, `pactum-scan` |
| [proof](https://github.com/kostyk348/proof) — verifiable claims (Rust) | **Proof** primitive: `pactum-proof` (plus command timeouts) |
| `sint_ua` memory | fact chain in `pactum-core` (`pactum facts`) |

What stayed: the honest-refusal philosophy, escrow economics (+1/−1/−5 reputation), the flat-file proof registry, WebSub-compatible hub semantics, ECDSA P-256 identity, the "letters" envelope idea. What changed: one language (Rust), one binary, one protocol spec (`acp/1`), and participants that no longer have to be agents.

## FAQ

**Why letters (RFC-822) instead of JSON over HTTP only?**
An envelope is transport-agnostic and human-inspectable. HTTP relay, file spool, and later SMTP all carry the same bytes; git diffs and reviews them for free.

**Who guarantees the executor didn't lie?**
Nobody on their word. The money is in escrow, the proof carries hashes and exit codes, the verification rules are in the contract. A wrong proof means a refund; silence means slashing.

**Can the customer take the money back after the work is done?**
No. After `submitResult` the only exits are rule-bound: release, refund, or slash.

**Is this really internet-scale, or a local toy?**
The architecture is global from day one: a public relay + nodes over HTTP(S). The demo runs in-process for reproducibility. TLS and discovery are roadmap items 2 and 5.

**Why not only agents?**
Because "who fulfills the contract" was never the interesting question. "Who holds the obligation and who proves it" is — and that's humans and services just as much.

## License

[Apache License 2.0](LICENSE). © 2026 Konstantin — concept and implementation.

Free use, modification and distribution with the notice preserved. Patent grant included: the code gives no grounds for patent claims against its users.
