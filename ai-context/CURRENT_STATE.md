# Текущее состояние (обновлено: 2026-09-08)

## Железо/среда
- Борт: **ROCK 5A, Armbian 26.8.3 trixie vendor 6.1.115**, 192.168.0.225
  (radxa/radxa, NOPASSWD sudo, ssh-ключи). gspca_ov534 in-tree,
  librknnrt 2.3.0 вручную в /usr/lib/aarch64-linux-gnu/, overlay
  rk3588-uart7-m2 через /boot/armbianEnv.txt, systemd synergy.service.
- Камеры: PS Eye `/dev/video-pseye` (1415:2000) + Arducam
  `/dev/video-arducam` (0c45:6366) — udev-алиасы обеим.
- FC: **GEPRC TAKER F405 BLS 60A V2 STACK** (полётник GEP-F405-HD V3),
  Betaflight 4.4.3 GEPRCF405, подключён по USB к ПК (COM4, VCP) и тремя
  проводами к гребёнке ROCK (пины 22/33/6). Configurator держит COM4
  эксклюзивно — для наших MSP-скриптов его отключать.

## Что работает (доказано на железе)
- Гибрид C: PS Eye GRBG 640×480 **60 FPS**; детекция YOLOv8 NPU core0
  (32 мс @640), трекинг NanoTrack NPU core1 (**6.2 мс**), e2e кадр→бокс
  **10.6 мс**.
- Стрим OSD push (:9000) + канал управления (:9010, ADR-016):
  lock/arm/**stop/unlock**/ping; режимы TRACK/ACQUIRE/LOST/**IDLE**.
- Операторский UI (egui): двойной клик = захват, «× СНЯТЬ ЗАХВАТ», АРМ в
  2 клика, запись .mjpg (durable), горячие клавиши Esc/F/R, «Папка записей».
- Валидация на эталонных видео: refvideo/RESULTS.md.
- Деплой: tools/deploy.sh (WSL кросс-сборка + glibc-strip + unit),
  дефолтный IP .225.
- Устойчивость: retry камеры внутри процесса, RestartSec=10.

## FC-интеграция (сессия 09-05..09-08)
- **Корневая причина «serial не сохраняется» найдена:** BF 4.4 при каждой
  загрузке сбрасывает ТОЛЬКО serialConfig, если isSerialConfigValid() не
  проходит: `MAX_MSP_PORT_COUNT=3` (VCP считается и обязан иметь MSP).
  Валидный конфиг = MSP на VCP + 2 UART. Валидный конфиг персистентен
  (проверено после ребута), `feature RX_MSP` персистентен.
- Доказано: борт/пины 22/33/кабель целы насквозь (дальнее эхо 29/30);
  все 6 UART FC проверены и как MSP-приёмники (молчание), и как
  передатчики LTM/MAVLink (0 байт на борт).
- **Единственное, что осталось:** текущие точки пайки на FC — не RX/TX
  ни одного UART (или холодная пайка, или земля не на G). Ждём от
  заказчика подписи падов → перенос пайки на пару `T<n>/R<n>` →
  `fc_finduart.py` закрывает связь → тест каналов при ARM
  (MSP_RC: центры 1500, ch3=1310, ch4=1950).

## Грабли (не повторять)
- **BF 4.4: не ставить MSP больше чем на VCP+2 UART** — молчаливый сброс
  serialConfig при каждой загрузке (выглядит как «не сохраняется»).
- **CLI Configurator: вставка блока ломает хвостовой `save`** (баг #5127,
  «ave» вместо save) — набирать save руками, проверять `serial` до и после.
- **Armbian: overlays только через /boot/armbianEnv.txt** — правка
  extlinux.conf убила загрузку (перепрошивка!).
- UART-порты FC открываются ТОЛЬКО при загрузке: runtime-изменение
  serialConfig невидимо до ребута (не «оживляй» конфиг без перезагрузки).
- Тесты UART на борте: писать в порт паттерном `printf > /dev/ttyS7` в
  фоне + `cat` — вариант с exec-fd и фоновым писателем молча не передаёт.
- librknnrt 2.3.0: NHWC у float-входов, fp16 неточен, opt=3 портит
  fusion, zero-copy → SIGSEGV (ADR-006/011).
- Входящие TCP к user-портам при активном NPU могут не проходить (ADR-009);
  ARP-ответ ≠ живой sshd.
- UVC пере-enumeration меняет /dev/videoN — только udev-алиасы.
- Windows: `ssh -f` умирает; git pull на борту сломан — доставка deploy.sh.

## Следующие шаги
1. Подписи падов FC от заказчика → перенос пайки → fc_finduart → `$M>` →
   MSP_RC при ARM → моторы без пропов (чеклист в wiring_gep_f405.md §9).
2. Ретест UI заказчиком: АРМ/fail-safe/unlock/запись в поле.
3. Безопасность (safety_compliance): WatchdogSec+sd_notify, токен канала.
4. Детектор мелких целей: дообучение/tile-инференс (RESULTS R1-R2).
