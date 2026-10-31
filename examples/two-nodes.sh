#!/usr/bin/env bash
# Сетевой сценарий ACP: релей + узел-заказчик + узел-исполнитель.
#
# Терминал 1: релей и заказчик; Терминал 2: исполнитель.
# Показывает: публикацию контракта, fan-out по capability, исполнение
# в песочнице, доказательство, высвобождение escrow.
set -euo pipefail

PORT_RELAY=9001
PORT_CUSTOMER=9002
PORT_EXECUTOR=9003

echo "── 1. Поднять релей (терминал 1) ──────────────────────────"
echo "  pactum hub serve --port $PORT_RELAY --store data/hub.json"
echo
echo "── 2. Поднять заказчика (терминал 1) ─────────────────────"
echo "  pactum node serve --port $PORT_CUSTOMER --role principal \\"
echo "      --keystore keys/customer.json"
echo
echo "── 3. Поднять исполнителя (терминал 2) ───────────────────"
echo "  pactum node serve --port $PORT_EXECUTOR --role executor \\"
echo "      --capabilities 'ci.compile.*,deploy.*' \\"
echo "      --keystore keys/executor.json"
echo
echo "── 4. Подписать исполнителя на релей ────────────────────"
echo "  curl -s http://127.0.0.1:$PORT_RELAY/relay/subscribe \\"
echo "      -H 'Content-Type: application/json' \\"
echo "      -d '{\"node_id\":\"executor\",\"capability\":\"ci.compile.*\",\"endpoint\":\"http://127.0.0.1:$PORT_EXECUTOR/inbox\"}'"
echo
echo "── 5. Опубликовать контракт от лица заказчика ───────────"
echo "  pactum contract new --capability ci.compile.rust \\"
echo "      --title 'Собрать wasm' --cmd 'cargo build --release' \\"
echo "      --git https://github.com/example/repo \\"
echo "      --budget 1.00 --unit USDC --out contract.json"
echo
echo "  curl -s http://127.0.0.1:$PORT_CUSTOMER/outbox \\"
echo "      --data-binary @contract.json"
echo
echo "── 6. Исполнитель: обработать входящие ──────────────────"
echo "  curl -s -X POST http://127.0.0.1:$PORT_EXECUTOR/process"
echo
echo "── 7. Заказчик: проверить доказательство и высвободить ──"
echo "  curl -s -X POST http://127.0.0.1:$PORT_CUSTOMER/process"
echo
echo "── 8. Посмотреть память и репутацию ─────────────────────"
echo "  curl -s http://127.0.0.1:$PORT_CUSTOMER/facts | head"
echo "  curl -s http://127.0.0.1:$PORT_CUSTOMER/health"
