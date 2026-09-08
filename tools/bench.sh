#!/bin/bash
# Регрессионный бенчмарк synergy на борту (фаза E): стандартный прогон
# и сравнение ключевых метрик с эталоном.
#
#   tools/bench.sh [борта-ip]           # прогон + печать метрик
#   tools/bench.sh --save               # записать эталон (первый запуск)
#
# Метрики: FPS, track_ms(p50/p95), e2e_ms(p50/p95), TRACKING%, score.
# Эталон: ~/synergy/bench_baseline.txt; деградация >10% — красным.

set -euo pipefail
IP="${2:-192.168.0.224}"
REMOTE_DIR="\$HOME/synergy"

run_on_board() {
  ssh -o BatchMode=yes "radxa@$IP" "cd $REMOTE_DIR && rm -rf data && \
    ./target/release/synergy --config bench.toml --duration 30 --demo-detect 2>/dev/null \
    | grep -aE 'ИТОГ|кадров|track|TRACKING' ; \
    python3 tools/telemetry_report.py data/telemetry.jsonl"
}

echo "== bench: стандартный прогон 30 с (камера 60 FPS, NPU-трекер) =="
OUT="$(run_on_board)"
echo "$OUT"

# Сохранение эталона
if [[ "${1:-}" == "--save" ]]; then
  ssh -o BatchMode=yes "radxa@$IP" "echo '$OUT' > $REMOTE_DIR/bench_baseline.txt"
  echo "== эталон сохранён =="
  exit 0
fi

# Сравнение с эталоном, если есть
BASE="$(ssh -o BatchMode=yes "radxa@$IP" "cat $REMOTE_DIR/bench_baseline.txt 2>/dev/null" || true)"
if [[ -n "$BASE" ]]; then
  echo; echo "== сравнение с эталоном =="
  for key in "FPS:" "track_ms" "e2e_ms"; do
    cur="$(echo "$OUT" | grep -o "$key[^|]*" | head -2 | tail -1 | grep -oE '[0-9]+\.?[0-9]*' | head -1)"
    ref="$(echo "$BASE" | grep -o "$key[^|]*" | head -2 | tail -1 | grep -oE '[0-9]+\.?[0-9]*' | head -1)"
    if [[ -n "$cur" && -n "$ref" ]]; then
      mark="OK"
      (( $(echo "$cur > $ref * 1.10" | bc -l 2>/dev/null || echo 0) )) && mark="↑ ВЫШЕ"
      (( $(echo "$cur < $ref * 0.90" | bc -l 2>/dev/null || echo 0) )) && mark="↓ ДЕГРАДАЦИЯ"
      echo "$key текущее=$cur эталон=$ref  [$mark]"
    fi
  done
else
  echo "эталона нет — запустите tools/bench.sh --save для фиксации"
fi
