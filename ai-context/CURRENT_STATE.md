# Текущее состояние (обновлено: 2026-09-04, ночная смена)

## Что работает (доказано на железе)
- Гибрид C: камера **PS Eye** `/dev/video-pseye` (udev-алиас VID:PID
  1415:2000), GRBG 640×480 **60 FPS**; детекция YOLOv8 NPU core0 (32 мс @640),
  трекинг NanoTrack NPU core1 (**6.2 мс**), e2e кадр→бокс **10.6 мс**.
- Стрим OSD push (борт→UI, :9000) + канал управления (:9010, JSON-строки,
  ADR-016): lock/arm/**stop/unlock**/ping; режимы TRACK/ACQUIRE/LOST/**IDLE**.
- Операторский UI (crates/operator-ui, egui): видео+оверлеи, двойной клик =
  захват, «× СНЯТЬ ЗАХВАТ», АРМ в 2 клика (4 с автосброс), СТОП,
  **запись .mjpg** (durable: flush 1 с, стоп с джойном писателя),
  горячие клавиши Esc/F/R, живой заголовок окна, «Папка записей».
- Коммандер: MSP v1 SET_RAW_RC поверх UART, валидирован на симуляторе;
  схема подключения полётника готова — docs/wiring_gep_f405.md
  (GEP-F405-HD V3 → UART7 пины 22/33, /dev/ttyS7, overlay uart7-m2).
- Устойчивость: камера недоступна → retry внутри процесса (не краш-луп;
  инцидент 2026-09-04: 129 рестартов уронили сеть на часы), RestartSec=10.
- Валидация на эталонных видео: 18 роликов через реальный NPU, скоринг
  против GT — refvideo/RESULTS.md (трекер ×5-7 покрытия, медиана 7-15 px;
  детектор 3-8% на целях 7×4 px; тепловизор разделяется по conf ≈0.45).
- Деплой: tools/deploy.sh (WSL кросс-сборка + glibc-strip + unit-файл),
  systemd synergy.service автозапуск.

## Ключевые файлы
- crates/{common,capture,rknn-sys,detector,nano-track,pipeline,streaming,
  commander,app,operator-ui} — 10 крейтов
- models/: .onnx (tract) + .rknn (int8 mmse); refvideo/: GT + скрипты
- tools/: viewer.py, deploy.sh, bench.sh, synergy.service
- docs/: SDD-SPEC, ROADMAP, HARDWARE_TEST_RESULTS, safety_compliance,
  wiring_gep_f405, sdd/decisions (ADR-001..018-a)

## Грабли (не повторять)
- librknnrt 2.3.0: float-входы мульти-входовых графов — NHWC (ADR-011);
  fp16 неточен; opt=3 портит fusion; zero-copy → SIGSEGV в DRM (ADR-006).
- «Кадр 3×3» был багом НАШЕГО JPEG-энкодера (планарный буфер в
  interleaved-кодер), камеры невиновны — ADR-018-a; защита оставлена.
- Входящие TCP к user-портам при активном NPU не проходят (ADR-009);
  при зависании ядра ARP ещё отвечает — не путать с «живым» бортом.
- Windows: `ssh -f` умирает; git pull на борту сломан — доставка deploy.sh.
- UVC/ov534 пере enumeration меняет /dev/videoN — только udev-алиас.

## Следующие шаги
1. Физическая сборка FC↔ROCK 5A по wiring-доке: overlay uart7-m2,
   конфиг commander на /dev/ttyS7, тест каналов MSP в Betaflight.
2. Ретест UI заказчиком: АРМ/fail-safe/unlock/запись в поле.
3. Безопасность (из safety_compliance): WatchdogSec+sd_notify, токен
   канала, авто-разарм при длительном LOST.
4. Детектор на мелких целях: дообучение/tile-инференс (refvideo/RESULTS R1-R2).
