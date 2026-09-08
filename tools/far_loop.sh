#!/bin/bash
# Дальнее эхо-тест кабеля: шлём маркеры SYN### в ttyS7 и считаем эхо.
# Пользователь скручивает ДВА СИГНАЛЬНЫХ провода на дальнем конце (у FC),
# землю не трогаем. Эхо есть = гребёнка ROCK + оба провода целы насквозь.
# ВАЖНО: паттерн записи именно такой (printf > DEV в фоне + cat DEV) —
# вариант с exec-fd и фоновым писателем НЕ передаёт (проверено 2026-09-08).
set -u
DEV=/dev/ttyS7
OUT=/tmp/far_loop.bin

sudoc() { sudo -n "$@" >/dev/null 2>&1 || echo radxa | sudo -S "$@" >/dev/null 2>&1; }

sudoc systemctl stop synergy
sleep 1
trap 'sudoc systemctl start synergy' EXIT

stty -F "$DEV" 115200 raw -echo -echoe -echok 2>/dev/null || true

( for i in $(seq 1 30); do printf 'SYN%03d\n' "$i" > "$DEV"; sleep 0.1; done ) &
timeout 5 cat "$DEV" > "$OUT" 2>/dev/null
wait

N=$(wc -c < "$OUT")
M=$(grep -ao 'SYN[0-9]\{3\}' "$OUT" | wc -l)
echo "байтов вернулось: $N; маркеров SYN: $M"
if [ "$M" -gt 0 ]; then
  echo "ОК: кабель и пины 22/33 работают насквозь"
else
  echo "ПРОВАЛ: эха нет — разрыв в кабеле или провода не на пинах 22/33"
fi
