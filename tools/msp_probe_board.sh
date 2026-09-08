#!/bin/bash
# Проба MSP-линка FC → ROCK 5A (UART7, /dev/ttyS7).
# Отправляет MSP_STATUS и печатает ответ: начало $M> (24 4d 3e) = связь есть.
# Использование: msp_probe_board.sh [device]     # по умолчанию /dev/ttyS7
set -u
DEV="${1:-/dev/ttyS7}"
OUT=/tmp/msp_probe.bin

sudoc() { sudo -n "$@" >/dev/null 2>&1 || echo radxa | sudo -S "$@" >/dev/null 2>&1; }

echo "== stop synergy (освобождаем $DEV)"
sudoc systemctl stop synergy
sleep 1

stty -F "$DEV" 115200 raw -echo -echoe -echok 2>/dev/null || true

# Вернуть сервис на место, даже если проба упала.
trap 'echo "== start synergy"; sudoc systemctl start synergy' EXIT

echo "== отправляю MSP_STATUS ($DEV, 115200)"
printf '\x24\x4d\x3c\x00\x65\x65' > "$DEV"
timeout 3 cat "$DEV" > "$OUT" 2>/dev/null || true

echo "== ответ FC:"
od -An -tx1z "$OUT" 2>/dev/null
if [ "$(head -c 3 "$OUT" 2>/dev/null | od -An -tx1 | tr -d ' \n')" = "244d3e" ]; then
  echo "ОК: получен \$M> — двусторонняя связь FC<->борт есть"
else
  echo "МОЛЧАНИЕ: ответа нет (проверь wiring и serial-конфиг FC)"
fi
