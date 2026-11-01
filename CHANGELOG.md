# Changelog

Все заметные изменения проекта документируются в этом файле.
Формат основан на [Keep a Changelog](https://keepachangelog.com/ru/1.1.0/).

## [1.1.0] — 2026-11-01

### Добавлено

- **Консоль-демо** (`docs/console/index.html`): интерактивная демонстрация ACP/1 в браузере —
  Scout → Digest → Auditor обмениваются настоящими письмами `acp/1`, хаб проверяет подписки
  challenge-ом и честно отказывает (`404` → `hub.mode=denied`, oversize → `413`), цепочка фактов
  считается настоящим SHA-256, доказательства перепроверяются по отпечатку FNV-1a-64, подделка
  истории ломает `verify()` и роняет репутацию на −5. Бюджет демо-контракта — 0.00 USDC:
  деньги опциональны. Один самодостаточный файл без сборки и зависимостей; ключи и подписи
  симулированы (`ecdsa-p256:sim:`), хеши и HMAC — настоящие.
- **Нотариус контента** — новый крейт [`pactum-witness`](crates/pactum-witness) и группа команд
  `pactum witness`: подписанные приёмники (ECDSA P-256, проверка офлайн), append-only журнал
  `receipts.jsonl` с хеш-цепочкой (повреждённый журнал сервис не поднимает), blob-хранилище
  с контрольной сверкой SHA-256, checkpoint-ы в хаб (топик `witness.head`), rate limit на IP,
  честные отказы (`400/404/413/429/502`), чтение URL с защитой от SSRF, лимитами и ручными
  редиректами. REST: `POST /notarize`, `POST /notarize/url`, `GET /receipts`, `GET /verify`,
  `GET /chain/head`, `GET /chain/export`, `GET /blobs`, `GET /health`. 36 тестов в крейте,
  132 по воркспейсу.
- **Деньги — под флагом**: feature `escrow` (включён по умолчанию) в `pactum-cli`;
  `cargo build --no-default-features` даёт бинарник без контрактов и узлов — id, хаб, сканер,
  proof, факты и witness остаются. В CI добавлена проверка обоих вариантов.
- **Dockerfile** для `pactum witness serve` (multi-stage, дистрибутивный образ).
- Страница Witness: [kostyk348.github.io/pactum/witness](https://kostyk348.github.io/pactum/witness/)
  и руководство [`docs/WITNESS.md`](docs/WITNESS.md).

## [1.0.0] — 2026-11-01

Первый объединённый релиз. Pactum — референс-реализация ACP
(Autonomous Contract Protocol): универсальный протокол честного обмена
обязательствами для людей, сервисов, устройств и агентов.

### Добавлено

- **Крейты ядра**: `pactum-core` (письма RFC-822, SHA-256, JSON-Schema-валидатор,
  цепочка фактов), `pactum-identity` (ECDSA P-256, keystore AES-GCM+PBKDF2).
- **Контракты**: `pactum-contract` — модель `pactum-contract/v1`, escrow-движок
  (Initiated → Accepted → Completed → Released/Refunded/Slashed), трейт
  `SettlementProvider`, репутация +1/−1/−5.
- **Потоки**: `pactum-hub` — подписки с арендой (≤24 ч), персистентная очередь
  ретраев (5 с → 1 ч, 8 попыток), честные отказы (404 → `hub.mode=denied`,
  исчерпание → degraded + 503, oversized → 413), HMAC-SHA256 подписи доставок.
- **Сканер**: `pactum-scan` — источники url/file/cmd, conditional GET,
  SHA-256-дедуп, вежливые интервалы, изменчивые источники только с явным
  `allow_volatile`, очередь кандидатов с ревью человека.
- **Доказательства**: `pactum-proof` — заявления с командой и отпечатком,
  нормализация таймингов, вердикты verified/moved/broken; **новое против
  оригинала**: тайм-аут команды (по умолчанию 120 с) — зависшая проверка
  ругается, а не подвешивает CI.
- **Узлы**: `pactum-node` — оркестратор, песочница-исполнитель (сбор
  exec_hash/exit-кодов/артефактов), REST API, capability-релей с fan-out
  и очередью на случай «подписчиков ещё нет».
- **CLI**: бинарник `pactum` — id / contract / hub / scan / proof / facts /
  node / demo.
- **Демо**: `pactum demo` — полный жизненный цикл сделки в одном процессе.
- **Спецификация**: `docs/PROTOCOL.md` (acp/1).
- **Референс-эскроу**: `contracts/hardhat/contracts/AgentEscrow.sol` + hardhat-тесты
  (унаследовано из acp-mvp без изменений логики).
- **CI/CD**: GitHub Actions — fmt, clippy (−D warnings), тесты, релизные
  бинарники linux/macos/windows по тегу.
- **Билингвальный README** (EN + RU) с перекрёстными ссылками.
- **Сайт**: двуязычный лендинг `docs/index.html` для GitHub Pages —
  переключатель RU/EN, раздел «деньги опциональны», карточки модулей и REST.

### Родословная

- [acp-mvp](https://github.com/kostyk348/acp-mvp) — контракты, escrow, релей,
  песочница, память (Python + Solidity) → `pactum-contract`, `pactum-node`,
  `pactum-core`, `contracts/`.
- [hubd](https://github.com/kostyk348/hubd) — хаб подписок и сканер (Python)
  → `pactum-hub`, `pactum-scan`.
- [proof](https://github.com/kostyk348/proof) — реестр заявлений (Rust)
  → `pactum-proof`.

### Изменено (ребрендинг)

- «Agent Contract Protocol» → «**Autonomous** Contract Protocol»: участник —
  не только агент (человек, сервис, устройство, организация).
- Заголовок писем `X-Agent-Protocol` → `X-Pactum-Protocol` (значение `acp/1`);
  совместимые заголовки `X-Escrow-Budget`, `X-Timeout-UTC` сохранены.
- Один язык реализации — Rust; один бинарник — `pactum`.

### Исправлено

- CI: `AgentEscrow.sol` перенесён в `contracts/hardhat/contracts/` — hardhat
  ищет исходники внутри корня проекта, из-за старого пути джоб
  `solidity tests` падал с `HH700: Artifact not found`. Зависимости
  зафиксированы `package-lock.json`, установка в CI — через `npm ci`.

[1.0.0]: https://github.com/kostyk348/pactum/releases/tag/v1.0.0
