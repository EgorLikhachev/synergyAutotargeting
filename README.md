# synergyAutotargeting

Гибридная система сопровождения цели **вариант C**: трекинг **каждый кадр** (NanoTrack на CPU)
+ детекция **раз в N кадров** (YOLOv8 на NPU RK3588S).

Синергия двух проектов:

| Компонент | Откуда | Что взято |
|---|---|---|
| Трекер NanoTrack + ONNX-модели | **bkb** | алгоритм (порт OpenCV TrackerNano на Rust/tract), модели `ntModel`, стабилизатор, паттерн handoff «детекция → трекер» |
| YOLOv8-декодер + RKNN-модель | **bkb** | `model_5_dynamic_rk3588.rknn`, постпроцессор `yolov8_utils.py` (порт на Rust), пороги 0.45/0.45 |
| SDD-описательные файлы | **Autotargeting** | формат docs/SDD-SPEC + sdd/decisions + progress.json |
| V4L2 direct capture | **Autotargeting** | `v4l2_direct.rs` (32→100 FPS против v4l-crate, ADR D-011) |
| Калман-фильтр | **Autotargeting** | `kalman.rs` (константная скорость, сглаживание между детекциями) |
| NPU zero-copy приёмы | **Autotargeting** | UINT8/NHWC вход + FLOAT32 выход через `rknn_set_io_mem`, NPU_CORE_0 (ADR D-007/D-010) |

## Архитектура (Rust-first)

```
камера (V4L2 MJPG) → decode RGB24 ─┬─ каждый кадр → NanoTrack (tract, CPU) ──┐
                                   │                                          ├→ гибрид (pipeline) → OSD/телеметрия
                                   └─ раз в N ──→ letterbox → YOLOv8 (NPU) ──┘
```

- `crates/rknn-sys` — FFI к librknnrt.so: in-process zero-copy инференс
  (замена base64-сокет-моста из Autotargeting, ADR-002).
- `crates/detector` — YOLOv8-декодер: автоопределение layout (6 выходов bkb с DFL
  или 1 выход Autotargeting с sigmoid), NMS.
- `crates/nano-track` — порт OpenCV TrackerNano на tract + стабилизатор bkb + Калман.
- `crates/capture` — прямой V4L2 ioctl захват.
- `crates/pipeline` — гибридная логика варианта C: выбор цели, IoU-гейт,
  реинициализация, форс-детекция при потере.
- `crates/app` — CLI `synergy`: конфиг TOML, OSD, JSONL-телеметрия, снапшоты.

Python и C++ в рантайм-пайплайне **не используются** (требование «Rust-first»);
C++ остаётся только как задокументированный фолбэк NPU-моста (ADR-001/002).

## Железо

Radxa ROCK 5A v1.2 (RK3588S, 16 ГБ) + USB-камера Arducam. Radxa Debian 12,
ядро vendor 6.1.84, rknpu2 2.3.0.

## Быстрый старт

```bash
# На борту (aarch64 Linux):
cargo build --release --features npu
cp config.example.toml config.toml   # поправьте /dev/videoX
./target/release/synergy --duration 60

# Локально без железа (синтетический источник, трекер на tract):
cargo run --release -- --synthetic --duration 10
```

Вывод: `data/frame_XXXXXX.jpg` (OSD-снапшоты), `data/telemetry.jsonl`, итоговая
статистика в stdout. Результаты на железе — в [docs/HARDWARE_TEST_RESULTS.md](docs/HARDWARE_TEST_RESULTS.md).

## Документация

- [docs/SDD-SPEC.md](docs/SDD-SPEC.md) — спецификация (единственный источник истины)
- [docs/sdd/decisions.md](docs/sdd/decisions.md) — журнал ADR
- [docs/sdd/progress.json](docs/sdd/progress.json) — трекер этапов
- [docs/SYNERGY_MAP.md](docs/SYNERGY_MAP.md) — карта заимствований из bkb/Autotargeting
- [docs/HARDWARE_TEST_RESULTS.md](docs/HARDWARE_TEST_RESULTS.md) — приёмочные тесты на железе
