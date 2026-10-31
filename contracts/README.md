# contracts/ — эталонный расчётный бэкенд (Solidity)

Эталонная реализация расчётов для сети ACP, унаследованная из `acp-mvp`.
Rust-ядро Pactum не зависит от блокчейна: в протоколе расчёты — это трейт
`SettlementProvider` (см. `crates/pactum-contract`). Встроенный провайдер
`sim` — детерминированный escrow-движок для локальных сделок и тестов.
Этот смарт-контракт — референс того же жизненного цикла на блокчейне.

## AgentEscrow.sol

Полный жизненный цикл задачи, един-в-один с Rust-движком:

```
createTask ──▶ Initiated ──acceptTask──▶ Accepted ──submitResult──▶ Completed
                                                    │
                        verifyAndRelease(success) ──┼──▶ Released (+репутация)
                        verifyAndRelease(false)  ───┘──▶ Refunded (−репутация)
   Accepted и тишина после дедлайна ── slashOnTimeout() ──▶ Slashed (−5 репутации)
```

Возможности:

* ERC-20 (USDC по умолчанию) и нативный эфир как fallback;
* слэшинг ставки исполнителя по тайм-ауту (`slashStake`, % бюджета);
* делёж royalty: `royaltyHuman` / `royaltyAgent` в процентах;
* накопленная репутация исполнителя в сети (`reputation` mapping);
* `messageId` связывает on-chain задачу с письмом-контрактом ACP.

## Как запустить тесты

Требуется Node.js 18+.

```bash
cd contracts/hardhat
npm install
npx hardhat test          # 10 контрактных тестов
npx hardhat node          # локальная сеть для экспериментов
npx hardhat run scripts/deploy.js --network localhost
```

## Как подключить к Pactum

Провайдер расчётов в контракте задаётся полем `settlement.provider`:

| provider | что это |
|---|---|
| `sim` | встроенный движок Pactum (по умолчанию) |
| `eip155:11155111:<address>` | сеть + адрес AgentEscrow (пример: Sepolia) |

Адаптер блокчейн-провайдера (web3-вызовы к этому контракту) — пункт
дорожной карты; интерфейс трейта уже стабилен и покрыт тестами.
