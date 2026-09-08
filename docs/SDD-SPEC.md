# SDD-SPEC — synergyAutotargeting

**Единственный источник истины по архитектуре.** Формат унаследован от
Autotargeting (docs/SDD-SPEC.md); журналы решений — в [sdd/decisions.md](sdd/decisions.md).

## 1. Назначение

Бортовая система сопровождения цели на RK3588S: получает кадры с USB-камеры,
детектирует объекты нейросетью на NPU, сопровождает выбранную цель трекером
на каждом кадре, выдаёт центр/бокс цели в телеметрию. Вариант **C** — гибрид.

## 2. Требования

- R1. Трекинг на каждом кадре (камера PS Eye GRBG 60 FPS), задержка кадр→бокс < 50 мс.
- R2. Детекция раз в N кадров (настраивается, базово 10 ≈ 3 Гц).
- R3. Автоматический (ре)захват цели после потери.
- R4. Headless-работа по SSH: OSD-снапшоты + JSONL-телеметрия.
- R5. Rust-first: Python/C++ в рантайме запрещены (ADR-001).
- R6. Сборка на борту: `cargo build --release --features npu`.

## 3. Компоненты

```
crates/
  common      — Frame/BBox/Detection/PixelFormat (все зависят)
  capture     — V4l2DirectSource (ioctl/MJPG/GRBG) + конверсии
  rknn-sys    — FFI librknnrt: RknnModel::load/infer (copy-mode, ADR-006)
  detector    — letterbox + YoloDecoder (layout YoloBranches-6 / SingleHead-1) + NMS
  nano-track  — NanoTracker (tract/RKNN, ADR-010) + Stabilizer + KalmanFilter2D
  pipeline    — HybridTracker: правила гибрида C, режимы Tracking/
                DetectAcquire/Lost/Idle (unlock)
  streaming   — MJPEG сервер + push (борт→зритель, ADR-009)
  control     — канал UI (ADR-016): JSON-строки, dead-man 1 c; при
                [control] token каждая строка обязана нести "auth"
                (ADR-020); чужой трафик dead-man не продлевает
  commander   — закон наведения + MSP v1 по UART (ADR-012); на стороне FC
                действует ограничение Betaflight 4.4: MSP максимум на
                VCP+2 UART (MAX_MSP_PORT_COUNT=3, иначе serialConfig молча
                сбрасывается при каждой загрузке) + feature RX_MSP;
                настройка и диагностика — tools/fc_*.py (см.
                docs/wiring_gep_f405.md §3, §5)
  app         — CLI synergy: конфиг, потоки, OSD, телеметрия, replay
  operator-ui — операторский egui-пульт (ADR-016): видео, захват,
                АРМ/СТОП/снять захват, запись .mjpg
```

## 4. Контракты

### VideoSource (capture)
`start() -> Receiver<Frame>`; канал глубиной = числу mmap-буферов (drop-old
политикой try_send — свежий кадр важнее старого).

### RknnModel (rknn-sys)
- `load(path, Some((w,h)))` — rknn_init → NPU_CORE_0 → при динамической модели
  `rknn_set_input_shapes` → форсировать вход UINT8/NHWC, выходы FLOAT32.
  Copy-mode: zero-copy через DRM в vendor-стеке падает SIGSEGV (ADR-006).
- `infer(rgb) -> Vec<Vec<f32>>` — копия входа в NPU-буфер (с учётом w_stride),
  rknn_run, чтение float32-выходов.
- Один экземпляр = один поток (не Sync).

### YoloDecoder (detector)
`from_output_dims(dims, config)` — авто-layout; `decode(outputs, dims, lb, w, h,
seq) -> Vec<Detection>` в координатах исходного кадра.

### NanoTracker (nano-track)
`init(img, bbox)` — шаблон 127; `update(img) -> BBox` — поиск 255 + пенальти
(scale/ratio/window) + плавное обновление lr. `tracking_score()` — качество.

### HybridTracker (pipeline)
- `wants_detection(frame_idx) -> bool` — по расписанию либо при потере.
- `on_detection(dets, img)` — выбор цели → init трекера.
- `on_frame(img) -> TargetState{mode, bbox, score, last_det_iou}`.

## 5. Конфигурация

TOML, см. config.example.toml: [camera] [detector] [tracker] [pipeline]
[output] [stream] [commander] [synthetic] [control] [logging].
Все пороги выведены; ключевые: `detect_every_n`, `min_track_score` (0.30),
`iou_confirm` (0.30), `lost_patience` (3).

## 6. Телеметрия

JSONL, строка на кадр: ts_ms, frame_seq, mode(TRACK/ACQUIRE/LOST/IDLE), x/y/w/h,
score, track_ms, det_ms, fps. Итог прогона — в stdout (средние значения,
распределение режимов).

## 7. Нефункциональные решения

- Процессы: один бинарник, детектор в отдельном std-потоке (каналы ёмкости 1 —
  never-blocking для основного цикла). Шина Zenoh из Autotargeting не нужна
  для одного процесса; при разбиении на компоненты — вернуться к D-014.
- Логирование: tracing, уровень через RUST_LOG.
- Надёжность процесса (ADR-019): systemd Type=notify + WatchdogSec=5,
  WATCHDOG=1 из кадровых циклов (sdnotify на std, вне systemd — no-op);
  зависание = рестарт в безопасном состоянии (armed=false).
- Тесты: юнит (декодер/стабилизатор/imgops/kalman) — без железа;
  синтетический режим (`--synthetic`) — полный цикл без камеры/NPU.

## 8. Открытые вопросы

1. Класс-набор model_5_dynamic_rk3588.rknn — уточнить эмпирически (M3).
2. Производительность tract на aarch64 (M4): если update > 25 мс — включить
   оптимизацию графов/рассмотреть C++ фолбэк (ADR-003).
3. Приоритизация целей (priority_classes) после выяснения классов.
4. ~~Quirk vendor-ядра: входящие TCP при активном NPU~~ — решён push-архитектурой
   (ADR-009/016): все каналы исходящие от борта, UI — приёмник (:9000/:9010).
5. ~~Push-режим как замена туннелю~~ — принят как основной (см. п.4).
