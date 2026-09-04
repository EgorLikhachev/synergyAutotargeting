#!/bin/bash
# Батч-прогон эталонных видео через --replay на борту (реальный NPU).
# Запуск НА БОРТУ: bash run_replays.sh   (останавливает сервис на время)
# Результаты: ~/synergy/data/runs/<время>-replay-<имя>/
set -u
cd ~/synergy
mkdir -p data/refvideo data/replay_runs

echo "== 1/3 проверка роликов"
ls data/refvideo/*.mjpg | wc -l

sudo -S systemctl stop synergy <<< "radxa" 2>/dev/null
sleep 2

echo "== 2/3 прогоны"
for f in data/refvideo/*.mjpg; do
    name=$(basename "$f" .mjpg)
    echo "--- $name"
    ./target/release/synergy --config config.toml --diag --replay "$f" --replay-rate 0 \
        --output "data/replay_runs/$name" 2>&1 | tail -3
done

echo "== 3/3 готово: $(ls data/replay_runs | wc -l) прогонов"
sudo -S systemctl start synergy <<< "radxa" 2>/dev/null
systemctl is-active synergy
