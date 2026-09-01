#!/bin/sh
# Подготовка ROCK 5A (Radxa Debian 12) под разработку synergy.
# Выполняется один раз на борту.
set -e

sudo apt-get update
sudo apt-get install -y \
  build-essential cmake pkg-config v4l-utils \
  python3-opencv python3-rknnlite2   # только для диагностики камеры/модели

# Rust (если ещё нет)
if [ ! -x "\$HOME/.cargo/bin/cargo" ]; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y \
    --default-toolchain stable --profile minimal
fi

# Проверка NPU-рантайма
ls -la /usr/lib/librknnrt.so /usr/include/rknn_api.h

# Проверка камеры
v4l2-ctl --list-devices

echo "Готово. Сборка: cargo build --release --features npu"
