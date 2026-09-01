# Карта синергии: что и откуда взято в synergyAutotargeting

Дата: 2026-09-01 (ночная сессия).

## Из bkb (sk/bkb, Python, полевой комплекс БПЛА)

| Артефакт bkb | Куда попал | Трансформация |
|---|---|---|
| `test_nano_cpu/nanoTracking.py` (NanoTracking) | `crates/nano-track/src/lib.rs` | Порт алгоритма OpenCV TrackerNano на Rust/tract; сам класс bkb — обёртка над cv2, портированы его гейты: счётчик потери, `_check_size`, `_edges_frame` (в pipeline) |
| `test_nano_cpu/ntModel/*.onnx` | `models/` | Как есть; backbone пересохранён в двух статических вариантах 127/255 (иначе tract конфликтует формами — ADR-003) |
| `test_nano_cpu/filter.py` (Stabilizer) | `crates/nano-track/src/stabilizer.rs` | Дословный порт |
| `utils/yolov8_utils.py` | `crates/detector/src/lib.rs` (decode_branches) | Порт DFL/box_process/post_process: NCHW→strides, 6-выходной layout, пороги |
| `model/model_5_dynamic_rk3588.rknn` | `models/` | Как есть — детектор на NPU |
| `config.yaml` (detector: 0.48/0.45, inputs 1088/640) | `config.example.toml` | Пороги взяты как базовые (0.45 для старта), input_size 640 |
| Паттерн `exchange_tracker()` (handoff) | `crates/pipeline` | Идея «детекция переинициализирует трекер» |
| Бенчмарк KCF/Affine/Nano (info.txt) | docs/sdd/decisions.md ADR-004 | Обоснование выбора NanoTrack |

## Из Autotargeting (verus/Autotargeting, Rust+C++)

| Артефакт Autotargeting | Куда попал | Трансформация |
|---|---|---|
| `docs/SDD-SPEC.md`, `sdd/decisions.md`, `sdd/progress.json` | `docs/` | Формат и подход spec-driven; содержимое новое |
| `crates/video-capture/src/v4l2_direct.rs` | `crates/capture/src/v4l2_direct.rs` | Дословно (проверено на 3 камерах включая Arducam) |
| `crates/video-capture/src/convert.rs` | `crates/capture/src/convert.rs` | Дословно |
| `crates/target-tracker/src/kalman.rs` | `crates/nano-track/src/kalman.rs` | Как есть (адаптирован тип BBox) |
| `rknn-bridge/src/rknn_model.cpp` | `crates/rknn-sys/src/safe.rs` | **Порт на Rust FFI**: rknn_init/set_core_mask/query/set_io_mem/run с тремя критическими приёмами (UINT8/NHWC, FLOAT32-выход, NPU_CORE_0). Ключевое улучшение синергии: in-process вместо base64-сокета (D-015: 96 мс сериализация) |
| `crates/yolov8/src/lib.rs` + `rknn_model.cpp` парсеры | `crates/detector` (decode_single) | Layout [1,4+Nc,A] + sigmoid-фикс логитов (D-010) |
| Топология «детектор → трекер» (at/detections → at/tracks) | `crates/pipeline` | Форма гибрида |
| HARDWARE_TEST_RESULTS.md | docs/ | Формат отчёта |

## Что осознанно НЕ взято

- PID/MSP/UART-наведение bkb (нет актуаторов на стенде; commander-скаффолд — M7)
- Zenoh-шина Autotargeting (для одиночного процесса избыточна; задокументирован
  путь расширения в SDD-SPEC §7)
- CUDA-трекеры bkb (нет NVIDIA на борту)
- Классы ThermalTracker (бибка — тепловизионная система; у нас USB-камера видимого
  диапазона, детекция полностью на NPU)
