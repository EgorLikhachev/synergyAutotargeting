# Текущее состояние (обновлено: 2026-09-10)

## Железо/среда
- Борт: **ROCK 5A, Armbian 26.8.3 trixie vendor 6.1.115**, 192.168.0.224
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
- **Soak 8ч ПРОЙДЕН (фаза E, 2026-09-09)**: 481/481 сэмплов, 0 пропусков,
  0 сторож-стойлов, 0 смертей; RSS 7,5ч ровно 50-51M; автостарт после
  чистого снятия питания подтверждён. Отчёт: `python tools/soak_report.py
  data/soak_20260909_0813.log`. Два наблюдения: (а) пик 80,4°C при
  включившемся трекинге (пассивное охлаждение — флаг для полевых);
  (б) несостоявшаяся утечка: +15M в конце = high-water mark после
  авто-захвата (mode=LOST без оператора), плато 66→68M за след. 4ч.

## FC-интеграция (2026-09-08: УПРАВЛЕНИЕ РАБОТАЕТ)
- **Контур наведения жив end-to-end на стенде**: FC подключён к USB-A
  борта (VCP, udev `/dev/tty-fc` 0483:5740), commander шлёт SET_RAW_RC
  38 Б ~23-30 Гц (throttle 1310 / arm 1950 — доказано strace), FC применяет
  кадры (доказано прямым чтением MSP_RC с борта). Настройка FC не нужна
  вовсе: VCP имеет MSP из коробки + `feature RX_MSP` уже включён.
- **Корневая причина «serial не сохраняется»** (закрыта): BF 4.4
  validateAndFixConfig при каждой загрузке сбрасывает ТОЛЬКО serialConfig,
  если isSerialConfigValid не проходит: MAX_MSP_PORT_COUNT=3 (VCP считается
  и обязан иметь MSP). Валид = MSP на VCP+2 UART.
- UART-провода (полевая задача №2): борт/пины 22/33/кабель доказаны
  (дальнее эхо 29/30), все 6 UART FC проверены в обе стороны (молчание) —
  точки пайки на падах FC не на UART (подписи так и не подтверждены).
  План возврата: мультиметр-прозвонка трёх трасс + serialpassthrough-скан.
- Борт: **DHCP, сейчас 192.168.0.224** (MAC 1A-DF-6B-72-8F-1A; заказчику
  рекомендована статическая аренда на роутере). Паттерн деградации сети:
  TCP умирает при живом пинге (~2 раза/день) — лечится ребутом борта;
  connect_timeout-фикс ускоряет переподключение пушей до секунд.

## Автономная отладка (по требованию заказчика)
- `tools/ui_sim.py` — headless-пульт по настоящим каналам :9000/:9010
  (keep-alive ping 300 мс, сценарии arm/stop/lock/unlock/wait/status).
- `tools/fc_rcpeek.sh` (MSP_RC peek поверх потока командера — заблокирован
  TIOCEXCL serialport; работает только при остановленном synergy),
  `tools/fc_wpos.sh` (fd-проба командера).
- Методика strace: `sudo timeout 3 strace -f -p $(pidof synergy) -e
  trace=write -s 48` в armed-окне — видны кадры $M< 38 Б.
- ~~Будущая фича: командер читает MSP и кладёт FC-телеметрию в статус~~ —
  РЕАЛИЗОВАНО 2026-09-08 (ADR-021): поле "fc"{online,rx_ok,flags,ch},
  индикатор в пульте, бит RC-видимости 0x80 определён эмпирически.

## Грабли (не повторять)
- **Keenetic-шлюз не отвечает на unprivileged ping**: у /usr/bin/ping нет
  suid/caps → из user-контекста ICMP идёт dgram-сокетом, шлюз молчит (root/
  raw — отвечает). ICMP-зонды из user-скриптов дают вечный gw=0 — сетевая
  проба только TCP (443/80 шлюза, см. soak.sh) или из root (netwatch).
  Выявлено 2026-09-09 по расхождению soak (gw=0) и netwatch (ping=1).
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

- **Аудит-проходка 2026-09-10** (c224a80..d62f6a8): ранний отсев
  YOLO-декодера (бит-идентичен), Cow-Img без клонов кадра, zune-jpeg,
  событийный repaint пульта, построчный демозаик (паритет доказан).
  Незакрыто: реальный замер на борту (bench до/после + grbg_demosaic_bench
  — на x86 демозаик дал лишь ~7%, NEON решать по борд-числам).

## Следующие шаги
1. Ретест UI заказчиком (Этап 3, ~30 мин): АРМ/fail-safe/unlock/запись +
   новый индикатор FC с полосками эха. Токен включать в момент ретеста:
   `ssh radxa@192.168.0.224 'bash ~/synergy/tools/enable_token.sh
   bench-synergy'`, пульт — через dist/operator-ui/run.bat.
2. Сбор данных R1 (заказчик, docs/data_collection.md): записи в
   dataset/raw/ + `python tools/dataset_check.py dataset/raw`.
3. Полевые — по чеклисту wiring_gep_f405.md §9 (после ретеста); следить
   за температурой: 80°C уже при стендовом трекинге.
4. R1-трейн — когда наберётся 15-20 роликов (метрики до/после —
   refvideo/run_replays.sh + replay_score.py).
