#!/bin/bash
# Заглянуть в MSP_RC полётника, пока командер стримит RC (commander только
# ПИШЕТ порт — все ответы FC достаются этому читателю; конкурентных читателей
# нет). Печатает hex-дамп ответов; декодирование каналов — на стороне ПК.
# Использование: fc_rcpeek.sh [запросов]     # по умолчанию 3
set -u
D=/dev/tty-fc
N="${1:-3}"
OUT=/tmp/rcpeek.bin
rm -f "$OUT"
( timeout 2 cat "$D" > "$OUT" 2>/dev/null ) &
R=$!
sleep 0.2
for i in $(seq 1 "$N"); do
  printf '\x24\x4d\x3c\x00\x69\x69' > "$D"
  sleep 0.4
done
wait $R 2>/dev/null
od -An -tx1 "$OUT" 2>/dev/null
