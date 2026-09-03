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
  # WSL Ubuntu 24.04 линкуется против glibc 2.39, а борт (Debian 12) — 2.36:
  # вырезаем требования версий новее 2.36 (слабые pidfd_* из Rust std).
  python3 /mnt/c/dev/synergyAutotargeting/tools/strip_glibc_verneed.py \
    /mnt/c/dev/synergyAutotargeting/synergy_cross 36
" | tail -2

echo "== 2/4 доставка на борт =="
scp -q "$REPO/synergy_cross" "radxa@$IP:/home/radxa/synergy/synergy_new"
scp -q "$REPO/tools/usb_camera_reset.sh" "radxa@$IP:/home/radxa/synergy/tools_camera_reset.sh"

echo "== 3/4 атомарная замена + рестарт сервиса =="
ssh -o BatchMode=yes "radxa@$IP" '
  sudo -S systemctl stop synergy <<< "radxa" 2>/dev/null
  sleep 3
  cd ~/synergy && chmod +x synergy_new && mv synergy_new target/release/synergy
  mkdir -p ~/synergy/tools
  mv ~/synergy/tools_camera_reset.sh ~/synergy/tools/usb_camera_reset.sh
  chmod +x ~/synergy/tools/usb_camera_reset.sh
  # sudoers: единственный root-вызов борта — USB reset камеры (idempotent)
  sudo -S bash <<< "radxa" 2>/dev/null -c "
    grep -q synergy-camera /etc/sudoers.d/synergy-camera 2>/dev/null || {
      echo radxa ALL=(root) NOPASSWD: /home/radxa/synergy/tools/usb_camera_reset.sh \
        > /etc/sudoers.d/synergy-camera
      chmod 440 /etc/sudoers.d/synergy-camera
      visudo -c -f /etc/sudoers.d/synergy-camera
    }"
  sudo -S systemctl start synergy <<< "radxa" 2>/dev/null
  sleep 3
  systemctl is-active synergy'

echo "== 4/4 контроль (первые метрики) =="
sleep 5
ssh -o BatchMode=yes "radxa@$IP" 'tail -2 ~/synergy/data/telemetry.jsonl 2>/dev/null | head -2'
echo "готово"
