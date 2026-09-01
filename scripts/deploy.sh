#!/bin/sh
# Деплой репозитория на борту и сборка там.
# Использование: scripts/deploy.sh [user@host]
set -e
HOST="${1:-radxa@192.168.0.224}"
REMOTE_DIR="\$HOME/synergy"

echo "==> rsync -> $HOST:$REMOTE_DIR"
rsync -az --delete \
  --exclude 'target' --exclude '.git' --exclude 'data' --exclude 'data_local' \
  --exclude 'config.toml' \
  ./ "$HOST:$REMOTE_DIR/"

echo "==> удалённая сборка (release + npu)"
ssh "$HOST" "cd $REMOTE_DIR && ~/.cargo/bin/cargo build --release --features npu 2>&1 | tail -5"

echo "==> готово: $HOST:$REMOTE_DIR/target/release/synergy"
