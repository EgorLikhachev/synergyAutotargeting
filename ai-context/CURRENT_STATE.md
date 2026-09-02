# Текущее состояние (обновлено: 2026-09-03, ночная смена)

## Что работает (доказано на железе)
- Гибрид C: камера 640×480 **60 FPS** (режим 100 FPS даёт ~81), детекция
  YOLOv8 NPU core0 (32 мс @640), трекинг NanoTrack NPU core1 (**6.2 мс**),
  100% TRACKING на demo-цели, e2e кадр→бокс **10.6 мс** (медиана).
- Стрим OSD: push (борт→зритель, `--stream-push`, tools/viewer.py) и
  listen (через SSH-туннель, quirk ядра — см. ADR-009); `--record` в M-JPEG.
- Коммандер (фаза D): MSP v1 SET_RAW_RC поверх UART (порт bkb),
  PID+slew+deadband+свап осей, упреждение с фидфорвардом платформы;
  валидирован на симуляторе ±30 px; ждёт UART-overlay + полётник.
- Деплой: systemd synergy.service (автозапуск), кросс-компиляция WSL→aarch64
  (tools/deploy.sh), bench-эталон (tools/bench.sh): 60.6 FPS / 6.13 мс.

## Ключевые файлы
- crates/{common,capture,rknn-sys,detector,nano-track,pipeline,streaming,commander,app}
- models/: .onnx (tract) + .rknn (int8 mmse; голова int8 из-за quirk fp16)
- tools/: viewer.py, telemetry_report.py, convert_nanotrack.py, bench.sh,
  deploy.sh, synergy.service
- docs/: SDD-SPEC, ROADMAP, HARDWARE_TEST_RESULTS (все замеры), sdd/decisions (ADR-001..012)

## Грабли (не повторять)
- librknnrt 2.3.0: float-входы мульти-входовых графов — NHWC (ADR-011);
  fp16 неточен на устройстве; opt=3 портит fusion. Zero-copy → SIGSEGV
  в DRM (ADR-006, в бэклоге).
- Быстрый re-open UVC → кадр 3×3 (гвард на старте).
- Входящие TCP к user-портам при активном NPU не проходят (ADR-009).
- Windows: `ssh -f` умирает; git pull на борту сломан (репо приватное) —
  доставка через tools/deploy.sh (scp).

## Следующие шаги
1. Фаза A — эталонное видео/объект: валидация реальной детекции
   (tools/telemetry_report.py + detections.jsonl уже готовы).
2. Фаза D — железо: UART-overlay (пин от заказчика), полётник, тюнинг kp/kd.
3. Бэклог: H.264 (mpph264enc), zero-copy, мультицелевой режим.
