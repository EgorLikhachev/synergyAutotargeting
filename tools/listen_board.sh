#!/bin/bash
# Прослушка UART7 (/dev/ttyS7): сколько байтов пришло от FC и hex-дамп.
# Использование: listen_board.sh [сек]     # по умолчанию 3
set -u
SECS="${1:-3}"
DEV=/dev/ttyS7
OUT=/tmp/uart_listen.bin

sudoc() { sudo -n "$@" >/dev/null 2>&1 || echo radxa | sudo -S "$@" >/dev/null 2>&1; }

sudoc systemctl stop synergy
sleep 1
trap 'sudoc systemctl start synergy' EXIT

stty -F "$DEV" 115200 raw -echo -echoe -echok 2>/dev/null || true
timeout "$SECS" cat "$DEV" > "$OUT" 2>/dev/null || true
N=$(wc -c < "$OUT")
echo "байтов принято: $N"
if [ "$N" -gt 0 ]; then
  od -An -tx1z "$OUT" | head -20
fi
