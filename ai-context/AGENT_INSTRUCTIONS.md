# Инструкция агенту

- Репозиторий: synergyAutotargeting (Rust-first, ADR-001). Плата:
  radxa@192.168.0.224 (rock-5a, ~/synergy, systemd synergy.service).
- Сборка на борту: cargo build --release --features npu (~2.5 мин).
  Быстрее: tools/deploy.sh (кросс из WSL, ~1 мин).
- Перед изменениями: docs/SDD-SPEC.md; изменения поведения → ADR в
  docs/sdd/decisions.md; замеры → docs/HARDWARE_TEST_RESULTS.md.
- После каждого этапа: cargo test --workspace (52+ тестов),
  на борту tools/bench.sh (сравнение с bench_baseline).
- Стрим для наблюдения: python tools/viewer.py + на борту
  --stream-push <ip>:9000, смотреть http://127.0.0.1:9001/.
- Не использовать: zero-copy rknn (ADR-006), `let _ =` для закрытия
  каналов, `ssh -f` на Windows.
