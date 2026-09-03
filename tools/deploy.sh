#!/bin/bash
# Деплой synergy на борт через кросс-компиляцию в WSL (фаза E).
#   tools/deploy.sh [ip]     # сборка → strip → scp → рестарт сервиса
# Требуется: WSL Ubuntu с rustup (+ target aarch64), gcc-aarch64-linux-gnu,
# /root/aarch64-libs/librknnrt.so (см. docs/HARDWARE_TEST_RESULTS §13).
set -euo pipefail
IP="${1:-192.168.0.224}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"

echo "== 1/4 кросс-сборка (WSL, aarch64) =="
wsl -d Ubuntu -e bash -c "
  export PATH=\$PATH:/root/.cargo/bin
  cd /mnt/c/dev/synergyAutotargeting 2>/dev/null || cd '$REPO'
  CARGO_TARGET_DIR=/root/xtarget RKNN_LIB_DIR=/root/aarch64-libs \
    cargo build --release --target aarch64-unknown-linux-gnu --features npu
  aarch64-linux-gnu-strip -o /mnt/c/dev/synergyAutotargeting/synergy_cross \
    /root/xtarget/aarch64-unknown-linux-gnu/release/synergy
" | tail -1

echo "== 2/4 доставка на борт =="
scp -q "$REPO/synergy_cross" "radxa@$IP:/home/radxa/synergy/synergy_new"

echo "== 3/4 атомарная замена + рестарт сервиса =="
ssh -o BatchMode=yes "radxa@$IP" '
  sudo -S systemctl stop synergy <<< "radxa" 2>/dev/null
  sleep 3
  cd ~/synergy && chmod +x synergy_new && mv synergy_new target/release/synergy
  sudo -S systemctl start synergy <<< "radxa" 2>/dev/null
  sleep 3
  systemctl is-active synergy'

echo "== 4/4 контроль (первые метрики) =="
sleep 5
ssh -o BatchMode=yes "radxa@$IP" 'tail -2 ~/synergy/data/telemetry.jsonl 2>/dev/null | head -2'
echo "готово"
