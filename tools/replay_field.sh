#!/bin/bash
# Полевая запись пульта (records/*.mjpg) → прогон на борту через --replay
# с реальным NPU → метрики сопровождения (без GT: покрытие/оценка/режимы).
#
#   tools/replay_field.sh <файл.mjpg> [ip]
# Результат: refvideo/field_runs/<имя>/ (телеметрия + сводка на экране).
set -euo pipefail
FILE="${1:?использование: replay_field.sh <файл.mjpg> [ip]}"
IP="${2:-192.168.0.225}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
NAME="$(basename "$FILE" .mjpg)"

echo "== 1/4 заливка $NAME на борт"
ssh -o BatchMode=yes "radxa@$IP" 'mkdir -p ~/synergy/data/refvideo'
scp -q "$FILE" "radxa@$IP:~/synergy/data/refvideo/${NAME}.mjpg"

echo "== 2/4 прогон (сервис останавливается на время)"
ssh -o BatchMode=yes "radxa@$IP" "
  sudo -S systemctl stop synergy <<< 'radxa' 2>/dev/null
  sleep 2
  cd ~/synergy
  rm -rf data/replay_runs/$NAME; mkdir -p data/replay_runs/$NAME
  ./target/release/synergy --config config.toml --diag \
    --replay data/refvideo/$NAME.mjpg --replay-rate 0 \
    --output data/replay_runs/$NAME 2>&1 | tail -5
  sudo -S systemctl start synergy <<< 'radxa' 2>/dev/null
"

echo "== 3/4 выгрузка результатов"
OUT="$REPO/refvideo/field_runs/$NAME"
mkdir -p "$OUT"
scp -q "radxa@$IP:~/synergy/data/replay_runs/$NAME/**" "$OUT/" 2>/dev/null || \
  scp -q -r "radxa@$IP:~/synergy/data/replay_runs/$NAME/." "$OUT/"

echo "== 4/4 метрики"
python3 "$REPO/refvideo/field_metrics.py" "$OUT" || \
  echo "(field_metrics.py недоступен — метрики вручную из телеметрии)"
