# Инструкция агенту

- Репозиторий: synergyAutotargeting (Rust-first, ADR-001). Плата:
  radxa@192.168.0.224 (Armbian trixie, ~/synergy, systemd synergy.service).
- Сборка на борту: cargo build --release --features npu (~2,5 мин).
  Быстрее: tools/deploy.sh (кросс из WSL, ~1 мин).
- Перед изменениями: docs/SDD-SPEC.md; изменения поведения → ADR в
  docs/sdd/decisions.md; замеры → docs/HARDWARE_TEST_RESULTS.md.
- После каждого этапа: cargo test --workspace (63 теста),
  на борту tools/bench.sh (сравнение с bench_baseline).
- Стрим для наблюдения: python tools/viewer.py + на борту
  --stream-push <ip>:9000, смотреть http://127.0.0.1:9001/.
- FC (Betaflight 4.4.3, COM4 по USB): инструменты tools/fc_*.py и
  msp_query.py; Configurator должен быть ОТКЛЮЧЕН (держит порт).
  Главное ограничение: MSP максимум на VCP+2 UART (MAX_MSP_PORT_COUNT=3,
  иначе serialConfig молча сбрасывается при каждой загрузке) —
  docs/wiring_gep_f405.md §3.
- Диагностика UART на борту: msp_probe_board.sh / listen_board.sh /
  far_loop.sh (ssh, сами стоп/стартят synergy).
- Не использовать: zero-copy rknn (ADR-006), `let _ =` для закрытия
  каналов, `ssh -f` на Windows, exec-fd фоновую запись в ttyS*
  (молча не передаёт — только `printf > DEV` + `cat`).
