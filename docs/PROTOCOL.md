# ACP v1.0 — Autonomous Contract Protocol

> Спецификация протокола, реализуемая Pactum 1.0.0.
> Кратко по-русски: ACP — протокол честного обмена обязательствами между любыми
> автономными участниками. Соглашение — машиночитаемый контракт, доставка —
> подписки с честными отказами, «сделано» — доказательство, которое любой
> может перепроверить одной командой. Полная русская версия — в `README.ru.md`.

---

## 1. Scope

ACP is a protocol for honest exchange of obligations between **autonomous
participants**. A participant is anything that can hold a key and act on its
own: a human with a laptop, a service in a cluster, a device on a shelf, an
organization, or an AI agent. Nothing in the protocol requires any of the
parties to be an agent, and nothing requires them to be human.

The protocol is composed of four independent-but-composable primitives:

| Primitive | Lineage | What it solves |
|---|---|---|
| **Contract** | acp-mvp | "Do X, here is how I will verify it, here is what you get" |
| **Hub** (topics) | hubd | event distribution with honest refusal instead of silent loss |
| **Proof** | proof | claims ("tests pass", "backup done") anyone can re-verify |
| **Ledger** (fact chain) | sint-ua | node memory that resists tampering |

## 2. Terminology

- **Participant** — an autonomous party identified by an ECDSA P-256 key pair.
- **Node** — a process that speaks ACP on behalf of one or more participants.
- **Letter** — the universal message envelope (§3).
- **Capability** — a dotted ASCII path (`ci.compile.rust`) describing a skill
  or role. Wildcards: `*` (everything), `prefix.*` (subtree).
- **Principal** — the party publishing an obligation. **Executor** — the party
  fulfilling it. Roles are orthogonal to participant kind.
- **Escrow** — the settlement engine holding the budget until proof.
- **Topic** — a named event stream on a hub.

## 3. Letters

Every message is a text envelope in the spirit of RFC 822: headers, one empty
line, a JSON body. Letters are readable in any editor, diff- and git-friendly,
and transport-agnostic (HTTP relay, file spool, SMTP adapter — roadmap).

```
X-Pactum-Protocol: acp/1
X-Pactum-Kind: contract
X-Pactum-From: p85e8728ad6908ea
X-Pactum-Capability: ci.compile.rust
X-Escrow-Budget: 1.00 USDC
X-Pactum-Reply-To: http://192.0.2.10:8080/inbox
Date: 2026-11-01T12:00:00Z
X-Pactum-Signature: ecdsa-p256:...

{ "schema": "pactum-contract/v1", ... }
```

Headers:

| Header | Meaning |
|---|---|
| `X-Pactum-Protocol` | protocol and version, `acp/1` — required |
| `X-Pactum-Kind` | `contract` \| `proof` \| `event` \| `receipt` |
| `X-Pactum-From` | node id of the sender |
| `X-Pactum-Capability` | addressing for contract fan-out |
| `X-Escrow-Budget` | display copy of the budget (from acp-mvp, kept) |
| `X-Timeout-UTC` | display copy of the deadline (from acp-mvp, kept) |
| `X-Pactum-Reply-To` | where proof letters should be delivered |
| `X-Pactum-Signature` | `ecdsa-p256:<128 hex>`, r‖s over the canonical bytes |

**Signing.** Canonical bytes = all headers except `X-Pactum-Signature` in
sorted order (`Name: value\n`), an empty line, then the compact JSON body.
The signature is ECDSA P-256 over those bytes (SECP256R1, as in acp-mvp).

**Node id.** `p` + first 16 hex chars of SHA-256 over the uncompressed SEC1
public key (65 bytes). Example: `p85e8728ad6908ea`.

## 4. Contracts

Contract format: `pactum-contract/v1`. JSON body of a `contract` letter,
validated against the embedded schema both before sending and after receiving.

```json
{
  "schema": "pactum-contract/v1",
  "schema_version": "1.0.0",
  "contract_id": "c-0c0ffee12345",
  "intent": {
    "type": "ci.compile.rust",
    "title": "Build wasm artifacts",
    "source": { "type": "inline | git | url | file", "data": "...", "ref": "..." }
  },
  "verification": {
    "tests": [ { "cmd": "cargo test", "timeout_s": 120 } ],
    "artifacts_expected": [ { "path": "target/*.wasm", "min_size": 1024 } ]
  },
  "settlement": {
    "provider": "sim",
    "budget": { "amount": "1.00", "unit": "USDC" },
    "on_success": "release_escrow",
    "on_failure": "refund",
    "royalty_split": { "principal": 90, "executor": 10 },
    "auto_verify": true,
    "slash_stake_percent": 10
  },
  "parties": { "principal": "p...", "executor": null },
  "deadline_utc": "2026-12-31T23:59:59Z"
}
```

Rules:

- `intent.type` must be a dotted ASCII path (it travels in HTTP headers).
- `royalty_split.principal + royalty_split.executor == 100`.
- Amounts are strings: no floating-point money, no fixed currency.
- Verification commands are executed by the executor's sandbox; "the check
  must fail" is legal — set expectations accordingly in your workflow.

### Lifecycle

```
Initiated ──accept──▶ Accepted ──submit──▶ Completed ──verify(true)──▶ Released
    │                    │                      └────verify(false)──▶ Refunded
    │                    └──(deadline passed)──▶ Slashed
    └── cancel before accept ──▶ Cancelled
```

Reputation deltas (inherited from acp-mvp): success **+1**, failed
verification **−1**, silence past deadline **−5**.

### Settlement

`settlement.provider` names the escrow backend:

- `sim` — built-in deterministic engine (local deals, tests, CI);
- external providers implement the `SettlementProvider` trait; the reference
  Solidity implementation lives in [`contracts/`](../contracts/README.md)
  (`createTask → acceptTask → submitResult → verifyAndRelease / slashOnTimeout`).

## 5. Hub protocol

WebSub-in-spirit (W3C), as implemented by hubd. All endpoints live under
`/hub/*` on a hub node.

| Method | Path | Body |
|---|---|---|
| POST | `/hub/subscribe` | WebSub form (`hub.mode=subscribe&hub.topic=…&hub.callback=…&hub.lease_seconds=…&hub.secret=…`) or JSON with the same keys |
| POST | `/hub/unsubscribe` | same shape, `hub.mode=unsubscribe` |
| POST | `/hub/publish?topic=T` | raw event bytes |
| POST | `/hub/retry` | — (one retry pass) |
| GET | `/hub/subscriptions` | list, secrets never exposed |
| GET | `/hub/journal` | last 50 journal entries |
| GET | `/hub/healthz` | liveness + counters |

Rules:

- **Challenge.** On subscribe, the hub GETs
  `callback?hub.challenge=R&hub.topic=T`; the subscriber must answer `200`
  echoing `R`.
- **Delivery.** POST event bytes to the callback with
  `X-Hub-Signature: sha256=<hex>` = HMAC-SHA256(secret, body) when a secret
  was set.
- **Honest refusal.** Subscriber answers `404` = "not for me": the hub marks
  the subscription `denied` and notifies it with a `hub.mode=denied` payload.
  No silent dropping.
- **Retries.** Failed deliveries are queued: first retry after 5 s, doubling
  up to 1 h, 8 attempts max, queue persisted on disk. Exhausted retries mark
  the subscription `degraded` and further publishes to it fail with `503`.
- **Size limit.** Bodies larger than 256 KiB are rejected with `413`.
- **Leases.** Maximum 24 h; renewal = re-subscribe. Expired subscriptions are
  removed on maintenance ticks.

## 6. Scanner sources

A watchlist is a JSON file. Three source kinds:

```json
{
  "sources": [
    { "name": "blog",     "url": "https://blog.rust-lang.org/feed.xml", "interval_s": 600 },
    { "name": "report",   "path": "reports/daily.json",  "topic": "ops.reports" },
    { "name": "diskfree", "cmd": "df -h /", "interval_s": 3600, "allow_volatile": true }
  ]
}
```

- URL sources use conditional GET (`If-None-Match` / `If-Modified-Since`);
- content is deduplicated by SHA-256;
- polite defaults: per-source interval (300 s) and ≥1 s between requests to
  the same host;
- command sources must be explicitly marked `allow_volatile` — otherwise
  their "changes" are skipped with a visible explanation;
- collection and delivery are separated: candidates go to a JSONL queue for
  human review (`new → accepted | rejected` with a reason), only then to a hub.

## 7. Proofs

A claim is a phrase someone said and a command that checks it. Registry: a
directory of flat text files (`proofs/0001.proof`) — git-friendly, readable.

```
id=1
claim=80 тестов зелёные
cmd=python3 -m unittest discover
expect=0
timeout=120
status=verified
digest=af63dc4c8601ec8c
excerpt=OK (80 tests)
```

- `add` runs the command immediately; unverified claims are never stored;
- `expect` makes "the check must fail" a legal proof;
- the fingerprint is FNV-1a 64 over **normalized** output: timing numbers
  (`0.012s`, `1.4s`, `12s`) become `<t>`;
- verdicts: `verified` / `moved` (output changed — rewrite the claim) /
  `broken` (exit code differs);
- `check` exits `1` if anything is broken — suitable for CI and cron;
- commands have a hard timeout (default 120 s): a hanging check fails loudly
  instead of hanging forever (an improvement over the original `proof`).

## 8. Fact chain (node memory)

Each fact is a block: `{seq, ts, register, text, contract_id?, prev_hash,
hash}`, where `hash = SHA256(seq|ts|REGISTER|text|contract_id|prev_hash)`.
Registers are epistemic and participant-agnostic: `SENSE`, `FACT`, `LOGIC`,
`OPINION`, `ACTION`. `verify()` walks the chain in O(n); any tampering breaks
it.

## 9. Security model

| Threat | Mitigation |
|---|---|
| Forged proof | exec_hash = SHA256(stdout+stderr), exit codes, artifact checks; auto-verify by contract rules |
| Executor disappears | deadline + `slashOnTimeout` + reputation −5 |
| Budget stolen | escrow holds funds until proof; release is rule-bound |
| Spam | minimum budget, capability subscriptions, reputation |
| Tampered history | hash chain (§8) |
| Forged letters | ECDSA P-256 signatures over canonical bytes (§3) |

**Honest limitations of v1.0:** the sandbox is local process isolation with
timeouts, not a TEE — for hostile code use docker/TEE backends (the
`Sandbox` trait is the seam); relay has no TLS by itself — put it behind TLS
or run on a trusted network; keystore uses AES-256-GCM + PBKDF2-HMAC-SHA256
(200k iterations) when a passphrase is provided.

## 10. Versioning and lineage

- Protocol: `acp/1` (the `X-Pactum-Protocol` header). Compatible in spirit
  with acp-mvp headers (`X-Escrow-Budget`, `X-Timeout-UTC` kept).
- Implementation: Pactum crates are versioned together (1.0.0).
- Lineage: acp-mvp (contracts, escrow, relay, sandbox, memory), hubd
  (subscription hub, scanner, honest refusals), proof (claims registry).
